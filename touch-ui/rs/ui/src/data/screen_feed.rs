//! luyaz-core's GET /v2/screen (src/screen_feed.c): the home signal card's and the status
//! bar's conclusions, computed by core from the snapshot the screen shows; read once per new
//! snapshot (at most every 300 ms; after a failure every 5 s), and every 5 s anyway (a stuck
//! executor never moves the snapshot, and exec_age_ms is how the screen sees that, E4).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::http;
use super::util::mono_ms;
use crate::json;
use crate::net_view;
use ui_model::net_view_t;

pub const SF_NONE: i32 = 0;
pub const SF_OK: i32 = 1;
pub const SF_OLD_DATAD: i32 = 2;
pub const SF_FAILING: i32 = 3;
/// the executor has not moved for this long: 数据服务没响应
pub const SF_STUCK_MS: i64 = 20000;
const SF_IO_MS: i32 = 300;

/// The timings (screen_feed.c's defines; tests shorten them).
#[derive(Clone, Debug)]
pub struct SfCfg {
    pub port: u16,
    pub gap_ms: i64,
    pub retry_ms: i64,
    pub stale_ms: i64,
    pub refresh_ms: i64,
}

impl Default for SfCfg {
    fn default() -> Self {
        SfCfg { port: 9460, gap_ms: 300, retry_ms: 5000, stale_ms: 30000, refresh_ms: 5000 }
    }
}

#[derive(Default)]
pub struct ScreenFeed {
    pub cfg: SfCfg,
    view: net_view_t,
    have: bool,
    status: i32,
    last_try: i64,
    /// behind = a newer snapshot than the view
    behind_since: i64,
    version: u64,
    view_version: u64,
    /// -1: not said (older core)
    exec_age: i64,
    /// the reply's "op" (write transactions), "" if none
    op: String,
}

impl ScreenFeed {
    pub fn new(cfg: SfCfg) -> ScreenFeed {
        ScreenFeed { cfg, exec_age: -1, ..Default::default() }
    }

    fn fetch(&mut self) -> bool {
        let Some(req) = http::build("GET", "/v2/screen", "127.0.0.1", "", None, 160) else { return false };
        let Some(fd) = http::connect_tcp("127.0.0.1", self.cfg.port, SF_IO_MS) else {
            self.status = SF_FAILING;
            return false;
        };
        // still enough with every *_en field (core screen/tests.rs checks the worst case)
        let r = http::exchange(fd, req.as_bytes(), 16384, SF_IO_MS);
        if r.status == 404 {
            self.status = SF_OLD_DATAD;
            self.have = false;
            return true;
        }
        let parsed = match (&r.body, r.status == 200 && !r.truncated) {
            (Some(b), true) => json::get(b, "net", 8192).map(|n| net_view::parse(&n)).filter(|p| p.0).map(|p| (b.clone(), p.1)),
            _ => None,
        };
        let Some((body, v)) = parsed else {
            self.status = SF_FAILING;
            return false;
        };
        let age = json::get_int(&body, "exec_age_ms", -1);
        let op = json::get(&body, "op", 8192).filter(|o| o.first() == Some(&b'{')).map(|o| String::from_utf8_lossy(&o).into_owned()).unwrap_or_default();
        let changed = !self.have || v != self.view || op != self.op || (age >= SF_STUCK_MS) != (self.exec_age >= SF_STUCK_MS);
        self.exec_age = age;
        self.op = op;
        self.view = v;
        self.have = true;
        self.status = SF_OK;
        changed
    }

    /// Each refresh with the core's snapshot version. true = the view changed.
    pub fn poll(&mut self, version: u64) -> bool {
        let t = mono_ms();
        let was = self.net().is_some();
        let ok = self.status == SF_OK || self.status == SF_NONE;
        let mut changed = false;
        self.version = version;
        if self.view_version != self.version || !self.have {
            let gap = if ok { self.cfg.gap_ms } else { self.cfg.retry_ms };
            if self.behind_since == 0 {
                self.behind_since = t;
            }
            if self.last_try == 0 || t - self.last_try >= gap {
                self.last_try = t;
                changed = self.fetch();
                if self.status == SF_OK {
                    self.view_version = self.version;
                    self.behind_since = 0;
                }
            }
        } else {
            // the same snapshot: fetch again now and then anyway (exec_age_ms, the write
            // transaction's phase); after a failure no faster than retries
            let gap = if ok || self.cfg.retry_ms < self.cfg.refresh_ms { self.cfg.refresh_ms } else { self.cfg.retry_ms };
            if t - self.last_try >= gap {
                self.last_try = t;
                changed = self.fetch();
            }
        }
        changed || was != self.net().is_some()
    }

    /// The latest view; None before the first good reply, while core answers 404, or when
    /// the snapshot moved on and fetching its view has failed for stale_ms.
    pub fn net(&self) -> Option<&net_view_t> {
        if !self.have {
            return None;
        }
        if self.view_version != self.version && self.behind_since != 0 && mono_ms() - self.behind_since > self.cfg.stale_ms {
            return None;
        }
        Some(&self.view)
    }

    pub fn status(&self) -> i32 {
        self.status
    }

    pub fn exec_age(&self) -> i64 {
        if self.have { self.exec_age } else { -1 }
    }

    pub fn stuck(&self) -> bool {
        self.have && self.exec_age >= SF_STUCK_MS
    }

    pub fn op(&self) -> &str {
        if self.have { &self.op } else { "" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    struct Fake {
        stop: Arc<AtomicBool>,
        h: std::thread::JoinHandle<()>,
    }

    fn fake(port: u16, status: i32, extra: &str, hits: Arc<AtomicUsize>) -> Fake {
        let l = TcpListener::bind(("127.0.0.1", port)).unwrap();
        l.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let body = format!(r#"{{"v":1,"ts":1,"net":{}{extra}}}"#, net_view::test_views()[0].1);
        let h = std::thread::spawn(move || {
            while !s2.load(Ordering::Relaxed) {
                let Ok((mut c, _)) = l.accept() else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                c.set_nonblocking(false).unwrap();
                let mut rq = [0u8; 1024];
                let _ = c.read(&mut rq);
                hits.fetch_add(1, Ordering::SeqCst);
                let out = if status == 200 {
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len())
                } else {
                    format!("HTTP/1.1 {status} X\r\nContent-Length: 0\r\n\r\n")
                };
                let _ = c.write_all(out.as_bytes());
            }
        });
        Fake { stop, h }
    }

    fn stop(f: Fake) {
        f.stop.store(true, Ordering::Relaxed);
        f.h.join().unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }

    // tests/screen_feed_test.c
    #[test]
    fn one_fetch_per_snapshot_and_the_stale_limit() {
        crate::lang::set_en(false);
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let mut s = ScreenFeed::new(SfCfg { port, gap_ms: 0, retry_ms: 0, stale_ms: 300, refresh_ms: 200 });
        assert!(s.net().is_none() && s.status() == SF_NONE);

        // an old core without /v2/screen
        let hits = Arc::new(AtomicUsize::new(0));
        let f = fake(port, 404, "", hits.clone());
        s.poll(1);
        assert!(s.net().is_none() && s.status() == SF_OLD_DATAD);
        stop(f);

        // a good reply
        let f = fake(port, 200, "", hits.clone());
        hits.store(0, Ordering::SeqCst);
        assert!(s.poll(1));
        assert!(s.net().is_some_and(|v| v.story.headline == "顺畅") && s.status() == SF_OK);
        // the same snapshot: no more requests
        s.poll(1);
        s.poll(1);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // a new snapshot: one more
        s.poll(2);
        assert!(hits.load(Ordering::SeqCst) == 2 && s.net().is_some());
        stop(f);

        // core gone, the snapshot moved on: the old view stays a while, then not
        s.poll(3);
        assert!(s.net().is_some() && s.status() == SF_FAILING);
        std::thread::sleep(Duration::from_millis(400));
        s.poll(3);
        assert!(s.net().is_none());

        // back: live again
        let f = fake(port, 200, "", hits.clone());
        assert!(s.poll(3) && s.net().is_some() && s.status() == SF_OK);
        stop(f);

        // the snapshot did not move: an unchanged view is current, however old
        std::thread::sleep(Duration::from_millis(400));
        s.poll(3);
        assert!(s.net().is_some());

        // E4: exec_age_ms and op; an older core sends neither
        assert!(s.exec_age() == -1 && !s.stuck() && s.op().is_empty());
        let f = fake(port, 200, r#","op":{"rollback_enabled":false,"active":null,"last":null},"exec_age_ms":250"#, hits.clone());
        hits.store(0, Ordering::SeqCst);
        // the snapshot did not move, but it is fetched again after refresh_ms
        std::thread::sleep(Duration::from_millis(250));
        s.poll(3);
        assert!(hits.load(Ordering::SeqCst) == 1 && s.exec_age() == 250 && !s.stuck());
        assert!(s.op().starts_with(r#"{"rollback_enabled":false,"#));
        s.poll(3);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "not before refresh_ms");
        stop(f);
        // stuck: changed (the screen greys its controls)
        let f = fake(port, 200, r#","exec_age_ms":21000"#, hits);
        std::thread::sleep(Duration::from_millis(250));
        assert!(s.poll(3) && s.stuck() && s.op().is_empty());
        stop(f);
    }
}
