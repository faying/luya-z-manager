//! 健康与告警 (src/alerts.c): zte-agent's /api/alerts and /api/health (logged in), read when
//! the page opens and every 10 s while it is up; 全部已读.
// SPDX-License-Identifier: MIT

use super::core_api::CoreApi;
use super::util::{bytes_cap, find, find_byte, fnv, jstr, mono_ms, obj_end, objects, trunc, FNV0};
use crate::cf;
use crate::json;
use crate::lang::{self, tr};
use devui_model::{alert_item_t, health_item_t};

const AL_TTL_MS: i64 = 10000;
const AL_RESP_MAX: usize = 32768;
pub const ALERTS_MAX: usize = 12;
pub const HEALTH_MAX: usize = 8;

pub struct Alerts {
    items: Vec<alert_item_t>,
    hc: Vec<health_item_t>,
    /// -1 not read yet, -2 read but nothing came
    hc_n: i32,
    hc_ok: i32,
    unread: i32,
    err: String,
    was_active: bool,
    poll_ms: i64,
    sig: u32,
}

impl Default for Alerts {
    fn default() -> Self {
        Alerts { items: Vec::new(), hc: Vec::new(), hc_n: -1, hc_ok: 0, unread: 0, err: String::new(), was_active: false, poll_ms: 0, sig: 0 }
    }
}

/// Text the agent words in both languages: <key>_en in English when there and not empty.
fn json_text(o: &[u8], key: &str, cap: usize) -> String {
    if lang::is_en() {
        let v = jstr(o, &format!("{key}_en"), cap);
        if !v.is_empty() {
            return v;
        }
    }
    jstr(o, key, cap)
}

impl Alerts {
    /// events: the objects after the first '[' (each cut out on its own: json lookups only see
    /// the top level and would run on into the next one).
    pub fn parse_events(&mut self, arr: &[u8]) {
        self.items.clear();
        let Some(p) = find_byte(arr, b'[', 0) else { return };
        for o in objects(&arr[p..]).into_iter().take(ALERTS_MAX) {
            let kind = jstr(o, "kind", 40);
            let mut e = alert_item_t {
                seq: json::get_int(o, "seq", 0),
                time: json::get_int(o, "time", 0), // null → 0
                uptime: json::get_int(o, "uptime", 0),
                label: json_text(o, "label", 128), // zte-agent words it
                text: jstr(o, "text", 128),
                unread: (jstr(o, "unread", 8) == "true") as i32,
            };
            if e.label.is_empty() {
                // an agent from before 9-26
                e.label = bytes_cap(&cf!(tr("其他告警（%s）"), &kind), 128);
            }
            self.items.push(e);
        }
    }

    /// checks: [{level,id,label,detail}], only warn and bad kept; the array to its own ']'
    /// (crashlogs' objects follow it).
    pub fn parse_checks(&mut self, data: &[u8]) {
        self.hc.clear();
        self.hc_n = 0;
        self.hc_ok = 0;
        let Some(p) = find(data, b"\"checks\"", 0).and_then(|p| find_byte(data, b'[', p)) else { return };
        let mut end = p;
        let (mut depth, mut instr, mut esc) = (0i32, false, false);
        while end < data.len() && data[end] != 0 {
            let c = data[end];
            if instr {
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'"' {
                    instr = false;
                }
            } else if c == b'"' {
                instr = true;
            } else if c == b'[' {
                depth += 1;
            } else if c == b']' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            end += 1;
        }
        let mut q = p;
        while let Some(o) = find_byte(data, b'{', q) {
            let Some(e) = obj_end(data, o) else { break };
            if o > end {
                break;
            }
            let obj = &data[o..=e];
            let level = jstr(obj, "level", 8);
            if level == "ok" {
                self.hc_ok += 1;
            } else if self.hc.len() < HEALTH_MAX && (level == "warn" || level == "bad") {
                self.hc.push(health_item_t {
                    bad: (level == "bad") as i32,
                    id: jstr(obj, "id", 24),
                    label: json_text(obj, "label", 48),
                    detail: json_text(obj, "detail", 320),
                });
            }
            q = e + 1;
        }
        self.hc_n = self.hc.len() as i32;
    }

    fn load_health(&mut self, agent: &mut CoreApi) {
        let (code, b) = agent.api("GET", "/api/health", None, AL_RESP_MAX);
        match b.filter(|_| code == 200).and_then(|b| json::get(&b, "data", AL_RESP_MAX)) {
            Some(d) => self.parse_checks(&d),
            None => {
                // never read: say so instead of 读取中 forever; read before: keep it
                if self.hc_n == -1 {
                    self.hc_n = -2;
                }
            }
        }
    }

    fn load(&mut self, agent: &mut CoreApi) {
        let (code, b) = agent.api("GET", "/api/alerts", None, AL_RESP_MAX);
        if code == 0 {
            self.err = trunc(tr("连不上管理后台"), 96);
            return;
        }
        if code == 401 {
            self.err = trunc(tr("登录管理后台失败（密码不对？）"), 96);
            return;
        }
        let Some(data) = b.filter(|_| code == 200).and_then(|b| json::get(&b, "data", AL_RESP_MAX)) else {
            self.err = bytes_cap(&cf!(tr("读告警失败（HTTP %d）"), code), 96);
            return;
        };
        self.err.clear();
        self.unread = json::get_int(&data, "unread", 0) as i32;
        self.parse_events(&data);
    }

    /// active = the page is up on a lit screen: at once when opened, then every 10 s.
    pub fn poll(&mut self, agent: &mut CoreApi, active: bool) -> bool {
        let just_shown = active && !self.was_active;
        let t = mono_ms();
        self.was_active = active;
        if !active {
            return false;
        }
        if !just_shown && self.poll_ms != 0 && t - self.poll_ms < AL_TTL_MS {
            return false;
        }
        self.poll_ms = t;
        self.load(agent);
        self.load_health(agent);
        let mut h = fnv(FNV0, format!("{}/{}/{}/{}", self.items.len(), self.unread, self.hc_n, self.hc_ok).as_bytes());
        h = fnv(h, self.err.as_bytes());
        for e in &self.items {
            h = fnv(h, format!("{}/{}", e.seq, e.unread).as_bytes());
        }
        for c in &self.hc {
            h = fnv(h, c.id.as_bytes());
            h = fnv(h, c.detail.as_bytes());
        }
        if h == self.sig {
            return false;
        }
        self.sig = h;
        true
    }

    pub fn items(&self) -> &[alert_item_t] {
        &self.items
    }
    pub fn unread(&self) -> i32 {
        self.unread
    }
    pub fn error(&self) -> &str {
        &self.err
    }
    pub fn health_count(&self) -> i32 {
        self.hc_n
    }
    pub fn health_checked(&self) -> i32 {
        self.hc_ok
    }
    pub fn health(&self) -> &[health_item_t] {
        &self.hc
    }

    /// 全部已读: up to the newest (the list is newest first); read again at once.
    pub fn mark_all_read(&mut self, agent: &mut CoreApi) {
        let Some(first) = self.items.first() else { return };
        let js = format!("{{\"seq\":{}}}", first.seq);
        agent.api("POST", "/api/alerts/read", Some(&js), AL_RESP_MAX);
        self.poll_ms = 0;
        self.was_active = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // tests/alerts_test.c
    #[test]
    fn health_and_events() {
        lang::set_en(false);
        let mut a = Alerts::default();
        assert_eq!(a.health_count(), -1, "nothing read yet");
        let d = r#"{"bad":1,"checked_at":1790250000,"checks":[{"detail":"在广播","id":"wifi","label":"Wi-Fi","level":"ok"},{"detail":"没配置号码：后台挂了你不会知道","id":"sms","label":"短信告警","level":"warn"},{"detail":"x [a] \"q\" }{","id":"disk","label":"/data 空间","level":"bad"},{"detail":"能访问","id":"agent-http","label":"管理网页 :9090","level":"ok"}],"crashlogs":[{"file":"a.log","level":"warn","program":"p"}],"error":null,"warn":1}"#;
        a.parse_checks(d.as_bytes());
        assert_eq!(a.health_count(), 2, "two not-ok checks");
        assert_eq!(a.health_checked(), 2, "two ok checks counted");
        let h = &a.health()[0];
        assert!(h.id == "sms" && h.bad == 0 && h.label == "短信告警");
        let h = &a.health()[1];
        assert!(h.bad != 0 && h.id == "disk" && h.detail.contains("[a]"), "brackets/quotes in detail survive");
        a.parse_checks(br#"{"checks":[],"warn":0}"#);
        assert!(a.health_count() == 0 && a.health_checked() == 0);
        let cs: Vec<String> = (0..12).map(|i| format!(r#"{{"detail":"d","id":"c{i}","label":"L","level":"warn"}}"#)).collect();
        a.parse_checks(format!(r#"{{"checks":[{}]}}"#, cs.join(",")).as_bytes());
        assert_eq!(a.health_count(), HEALTH_MAX as i32);

        // events: the label comes from zte-agent; an older agent sends none
        a.parse_events(
            r#"[{"seq":3,"time":null,"uptime":42,"kind":"agent-crash","label":"管理后台意外退出，已自动重启","text":"t","unread":true},{"seq":2,"time":1782396733,"uptime":1,"kind":"x-new","text":"u","unread":false}]"#.as_bytes(),
        );
        assert!(a.items()[0].label == "管理后台意外退出，已自动重启" && a.items()[0].unread != 0 && a.items()[0].seq == 3);
        assert!(a.items()[1].label == "其他告警（x-new）" && a.items()[1].unread == 0);

        // English: label_en / detail_en; null or missing → the Chinese
        lang::set_en(true);
        a.parse_events(
            r#"[{"seq":5,"time":null,"uptime":1,"kind":"agent-crash","label":"管理后台意外退出","label_en":"Admin backend (zte-agent) exited unexpectedly","text":"t","unread":true},{"seq":4,"time":null,"uptime":1,"kind":"y","label":"中文","label_en":"","text":"u","unread":false}]"#.as_bytes(),
        );
        assert_eq!(a.items()[0].label, "Admin backend (zte-agent) exited unexpectedly");
        assert_eq!(a.items()[1].label, "中文");
        a.parse_checks(
            r#"{"bad":1,"checked_at":1,"checks":[{"detail":"没在运行","detail_en":"Not running","id":"screen","label":"触屏界面","label_en":"Screen UI","level":"bad"},{"detail":"空间不够","detail_en":null,"id":"disk","label":"/data 空间","label_en":null,"level":"warn"}],"crashlogs":[]}"#.as_bytes(),
        );
        assert!(a.health()[0].label == "Screen UI" && a.health()[0].detail == "Not running");
        assert!(a.health()[1].label == "/data 空间" && a.health()[1].detail == "空间不够");
        lang::set_en(false);
    }
}
