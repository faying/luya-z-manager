//! Component versions (manager docs/designs/component-versions.md): GET /v2/versions every 30 s
//! while 系统 is up; 「检查更新」 is POST /v2/versions/check, which the core finishes on its own
//! (up to 20 s; the answer comes with the next read).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::core_api::CoreApi;
use super::util::{mono_ms, wall};
use crate::cf;
use crate::json;
use crate::lang::tr;
use devui_model::ver_row_t;

const POLL_MS: i64 = 30000;
const IO_MS: i32 = 800;
const RESP_MAX: usize = 4096;

/// A string value; null and missing are None.
fn text(v: &[u8], key: &str, cap: usize) -> Option<String> {
    json::get_str(v, key, cap).filter(|s| s != "null" && !s.is_empty())
}

/// The rows and the 上次检查 line of a /v2/versions reply (None: not one).
pub fn parse(body: &[u8], now: i64) -> Option<(Vec<ver_row_t>, String)> {
    let arr = json::get(body, "components", 2048)?;
    let mut rows = Vec::new();
    let mut p = 0;
    while let Some((c, next)) = json::arr_next(&arr, p, 256) {
        p = next;
        rows.push(ver_row_t {
            name: text(&c, "name", 16).unwrap_or_default(),
            installed: text(&c, "installed", 16).unwrap_or_default(),
            latest: text(&c, "latest", 16).unwrap_or_default(),
            update: json::get(&c, "update", 8).is_some_and(|v| v == b"true") as i32,
        });
    }
    if rows.is_empty() {
        return None;
    }
    let at = json::get_int(body, "checked_at", 0);
    let failed = text(body, "error", 160).is_some();
    let line = if at <= 0 {
        tr("还没查过").to_string()
    } else {
        let d = (now - at).max(0);
        let when = if d < 120 {
            tr("刚刚").as_bytes().to_vec()
        } else if d < 7200 {
            cf!(tr("%ld 分钟前"), d / 60)
        } else if d < 172_800 {
            cf!(tr("%ld 小时前"), d / 3600)
        } else {
            cf!(tr("%ld 天前"), d / 86400)
        };
        let when = String::from_utf8_lossy(&when).into_owned();
        if failed { format!("{when} · {}", tr("检查失败")) } else { when }
    };
    Some((rows, line))
}

#[derive(Default)]
pub struct Versions {
    rows: Vec<ver_row_t>,
    checked: String,
    was_active: bool,
    last_try: i64,
}

impl Versions {
    /// active = 系统 is on a lit screen. true = what it shows changed.
    pub fn poll(&mut self, agent: &mut CoreApi, active: bool) -> bool {
        let t = mono_ms();
        let came_back = active && !self.was_active;
        self.was_active = active;
        if !active || (!came_back && self.last_try != 0 && t - self.last_try < POLL_MS) {
            return false;
        }
        self.last_try = t;
        let (code, b) = agent.api_ms("GET", "/versions", None, RESP_MAX, IO_MS);
        let Some((rows, line)) = b.filter(|_| code == 200).and_then(|b| parse(&b, wall())) else { return false };
        if rows == self.rows && line == self.checked {
            return false;
        }
        (self.rows, self.checked) = (rows, line);
        true
    }

    /// 「检查更新」: the core looks now; read again in a few seconds.
    pub fn check(&mut self, agent: &mut CoreApi) {
        agent.api_ms("POST", "/versions/check", None, RESP_MAX, IO_MS);
        self.checked = tr("正在检查…").to_string();
        // the next poll in 5 s
        self.last_try = mono_ms() - POLL_MS + 5000;
    }

    pub fn rows(&self) -> &[ver_row_t] {
        &self.rows
    }
    pub fn checked(&self) -> &str {
        &self.checked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_report() {
        crate::lang::set_en(false);
        let b = br#"{"components":[{"name":"core","installed":"1.0.0","latest":"1.1.0","update":true},{"name":"scripts","installed":null,"latest":null,"update":false}],"update_available":true,"checked_at":1000,"ok_at":1000,"error":null}"#;
        let (rows, line) = parse(b, 1000 + 3 * 3600).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].installed.as_str(), rows[0].latest.as_str(), rows[0].update), ("1.0.0", "1.1.0", 1));
        assert_eq!((rows[1].installed.as_str(), rows[1].update), ("", 0));
        assert_eq!(line, "3 小时前");
        let b = br#"{"components":[{"name":"core","installed":"1.0.0","latest":null,"update":false}],"checked_at":null,"error":null}"#;
        assert_eq!(parse(b, 5).unwrap().1, "还没查过");
        let b = br#"{"components":[{"name":"core","installed":"1.0.0","latest":"1.0.0","update":false}],"checked_at":100,"error":"down"}"#;
        assert_eq!(parse(b, 130).unwrap().1, "刚刚 · 检查失败");
        assert!(parse(b"{}", 0).is_none());
    }
}
