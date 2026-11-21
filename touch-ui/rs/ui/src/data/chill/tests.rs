//! how often /configs is read (170 KB on the device, only `mode` used),
//! the last mode kept when it comes back cut or not at all, a stopped core noticed from the
//! small per-round request. A fake mihomo on a thread
//! counts each request; time is stepped by editing the timestamps.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::*;
use crate::data::testsrv::{listen, read_req};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq)]
enum Cfg {
    Ok,
    Truncated,
    Drop,
}

struct Fake {
    stop: Arc<AtomicBool>,
    configs: Arc<AtomicUsize>,
    main: Arc<AtomicUsize>,
    h: Option<std::thread::JoinHandle<()>>,
}

fn reply(c: &mut TcpStream, body: &[u8]) {
    let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
    let _ = c.write_all(body);
}

/// /configs with the route-exclude list in front of `mode`, as on the device
fn configs_body(mode: &str, pad: usize) -> Vec<u8> {
    let mut b = br#"{"port":0,"tun":{"route-exclude-address":[""#.to_vec();
    b.extend(std::iter::repeat_n(b'1', pad));
    b.extend_from_slice(format!(r#""]}},"mode":"{mode}","log-level":"info"}}"#).as_bytes());
    b
}

impl Fake {
    fn start(l: TcpListener, cfg: Cfg, mode: &'static str) -> Fake {
        let stop = Arc::new(AtomicBool::new(false));
        let (configs, main) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (st, c2, m2) = (stop.clone(), configs.clone(), main.clone());
        l.set_nonblocking(true).unwrap();
        let h = std::thread::spawn(move || {
            while !st.load(Ordering::Relaxed) {
                let Ok((mut c, _)) = l.accept() else {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                };
                c.set_nonblocking(false).unwrap();
                let rq = read_req(&mut c);
                if rq.starts_with("GET /configs") {
                    c2.fetch_add(1, Ordering::SeqCst);
                    match cfg {
                        Cfg::Ok => reply(&mut c, &configs_body(mode, 1000)),
                        Cfg::Truncated => reply(&mut c, &configs_body(mode, SC_RESP_MAX + 40000)),
                        Cfg::Drop => {}
                    }
                } else if rq.starts_with("GET /group") {
                    reply(&mut c, br#"{"proxies":[]}"#);
                } else if rq.starts_with("GET /connections") {
                    reply(&mut c, br#"{"downloadTotal":0,"uploadTotal":0,"connections":[]}"#);
                } else if rq.starts_with("GET /proxies/DIRECT") {
                    reply(&mut c, br#"{"name":"DIRECT","type":"Direct"}"#);
                } else if rq.starts_with("GET /proxies/") {
                    m2.fetch_add(1, Ordering::SeqCst);
                    reply(&mut c, br#"{"type":"Selector","now":"DIRECT","all":["DIRECT"]}"#);
                }
            }
        });
        Fake { stop, configs, main, h: Some(h) }
    }
    fn c(&self) -> usize {
        self.configs.load(Ordering::SeqCst)
    }
    fn m(&self) -> usize {
        self.main.load(Ordering::SeqCst)
    }
    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.h.take().unwrap().join().unwrap();
    }
}

fn round(c: &mut Chill) {
    c.last_ms = 0;
    assert!(c.poll(true));
}

fn age_cfg(c: &mut Chill) {
    if c.cfg_ms != 0 {
        c.cfg_ms -= SC_CFG_TTL_MS;
    }
}

/// A luyaz-core that answers 200 {} to everything (the exit change goes through it).
fn agent_ok() -> CoreApi {
    let (l, port) = listen();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { break };
            read_req(&mut c);
            let _ = c.write_all(b"HTTP/1.0 200 OK\r\n\r\n{\"ok\":true,\"data\":{\"token\":\"t\"}}");
        }
    });
    let mut a = CoreApi::default();
    a.config(port);
    a
}

#[test]
fn configs_read_sparingly() {
    crate::lang::set_en(false);
    let (l, port) = listen();
    let mut c = Chill { conf_loaded: true, port, ..Default::default() };

    // /configs at most every 30 s, the rest every round
    let f = Fake::start(l, Cfg::Ok, "global");
    round(&mut c);
    assert!(c.online() && c.core_running);
    assert_eq!(f.c(), 1);
    assert!(c.mode_raw() == "global" && c.exit_raw() == "global");
    for _ in 0..10 {
        round(&mut c);
    }
    assert_eq!(f.c(), 1, "11 rounds (~22 s), one /configs");
    assert!(f.m() >= 11, "the liveness request went every round");
    let m = f.m();
    assert!(!c.poll(true), "the 2 s throttle");
    assert_eq!(f.m(), m);
    age_cfg(&mut c);
    round(&mut c);
    assert_eq!(f.c(), 2, "30 s on: due again");

    // back on screen: fetched at once
    assert!(!c.poll(false));
    assert!(c.poll(true));
    assert_eq!(f.c(), 3);

    // our own exit change: read back on the next two rounds
    let c0 = f.c();
    let mut a = agent_ok();
    assert!(c.set_exit(&mut a, "proxy"));
    assert_eq!(c.mode_raw(), "global", "the last /configs read remains authoritative");
    round(&mut c);
    assert_eq!(f.c(), c0 + 1);
    assert_eq!(c.mode_raw(), "global", "then what the core really says");
    round(&mut c);
    round(&mut c);
    assert_eq!(f.c(), c0 + 2);
    f.stop();

    // /configs cut: the last mode kept, still online, still 30 s
    let l = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let f = Fake::start(l, Cfg::Truncated, "direct");
    assert!(c.get("/configs").is_some() && c.last_truncated, "the fake really overflows");
    age_cfg(&mut c);
    let c0 = f.c();
    round(&mut c);
    assert_eq!(f.c(), c0 + 1);
    assert!(c.mode_raw() == "global" && c.online());
    round(&mut c);
    round(&mut c);
    assert_eq!(f.c(), c0 + 1, "no 2 s retry of a 300 KB fetch");
    f.stop();

    // /configs unanswered: the last mode kept, still online
    let l = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let f = Fake::start(l, Cfg::Drop, "direct");
    age_cfg(&mut c);
    let c0 = f.c();
    round(&mut c);
    assert_eq!(f.c(), c0 + 1);
    assert!(c.mode_raw() == "global" && c.online());
    f.stop();

    // the core stopped: offline on the next round; back: /configs at once
    round(&mut c);
    assert!(!c.online() && !c.core_running && c.core() == "已停止");
    let l = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let f = Fake::start(l, Cfg::Ok, "global");
    let c0 = f.c();
    round(&mut c);
    assert!(c.online() && f.c() == c0 + 1 && c.mode_raw() == "global");
    f.stop();
}

#[test]
fn config_profile_parses_active_and_rejects_incomplete_data() {
    let body = br#"{"ok":true,"data":{"active_id":"p-2","profiles":[{"id":"p-1","name":"Other","kind":"node_url"},{"id":"p-2","name":"SG2HOME \u56de\u5bb6","kind":"full_url"}]}}"#;
    assert_eq!(parse_config_profile(body), Some(("SG2HOME 回家".into(), Some("full_url".into()))));
    assert_eq!(parse_config_profile(br#"{"data":{"active_id":null,"profiles":[]}}"#), Some((String::new(), None)));
    assert_eq!(parse_config_profile(br#"{"data":{"active_id":"p-2","profiles":[]}}"#), None);
    assert_eq!(parse_config_profile(br#"{"data":{"active_id":null,"profiles":["#), None);
}

#[test]
fn config_profile_failed_read_retains_previous_result() {
    let mut p = ConfigProfile { name: Some("Previous".into()), kind: Some("local_yaml".into()), last_ms: 0 };
    let (l, port) = listen();
    let h = std::thread::spawn(move || {
        let (mut c, _) = l.accept().unwrap();
        let req = read_req(&mut c);
        assert!(req.starts_with("GET /v2/api/services/chill/configs"));
        reply(&mut c, br#"{"data":{"active_id":"missing","profiles":[]}}"#);
    });
    let mut agent = CoreApi::default();
    agent.config(port);
    assert!(!p.poll(&mut agent, true));
    h.join().unwrap();
    assert_eq!(p.name.as_deref(), Some("Previous"));
    assert_eq!(p.kind.as_deref(), Some("local_yaml"));
    assert!(!p.poll(&mut agent, true), "failed reads are throttled");
}

#[test]
fn names_for_the_device_fonts() {
    assert_eq!(sanitize_name("🇯🇵 JP2-大阪", 96), "JP2-大阪");
    assert_eq!(sanitize_name("🇯🇵 日本", 64), "JP 日本");
    assert_eq!(sanitize_name("🚀 节点选择", 96), "节点选择");
    assert_eq!(sanitize_name("🚀", 96), "-");
    assert_eq!(sanitize_name("a  b ", 96), "a b");
    assert_eq!(path_escape("🚀 节点", 384), "%F0%9F%9A%80%20%E8%8A%82%E7%82%B9");
    assert_eq!(size(1536), "1K");
    assert_eq!(size(5 * 1048576 + 7), "5M");
    assert_eq!(size(3 * 1073741824 / 2), "1.5G");
}

/// Every group gets a chip: 19 groups (more than the old 12/16 caps), main and AI first.
#[test]
fn groups_all_kept_main_and_ai_first() {
    let names = [
        "GLOBAL", "家里IP", "家里出口", "🇯🇵 日本", "🇯🇵 日本·自动", "🇸🇬 新加坡", "🇸🇬 新加坡·自动", "🇹🇼 台湾",
        "🇹🇼 台湾·自动", "🇺🇸 美国", "🇺🇸 美国·自动", "🍎 Apple", "🎯 手动节点", "🐟 漏网之鱼", "📞 VoWiFi",
        "📺 流媒体", "🤖 AI", "🚀 节点选择", "DIRECT-X",
    ];
    let mut j = String::from("{\"proxies\":[");
    for n in names {
        j += &format!("{{\"name\":\"{n}\",\"type\":\"Selector\"}},");
    }
    j += "]}";
    let mut c = Chill::default();
    c.parse_groups(j.as_bytes());
    assert_eq!(c.grp_raw.len(), names.len());
    assert_eq!(c.grp_raw[0], "🚀 节点选择");
    assert_eq!(c.grp_raw[1], "🤖 AI");
    assert_eq!(c.grp_raw.last().map(String::as_str), Some("GLOBAL"));
    assert!(c.grp_raw[c.grp_raw.len() - 5..c.grp_raw.len() - 1].iter().all(|g| g.ends_with("·自动")));
    assert!(names.len() < SC_MAX_GRP);
}

/// Auto groups: a 自动 row first; the tick follows the pin, not the node in use.
#[test]
fn auto_group_row_and_pin() {
    assert_eq!(auto_row_label("", "🇹🇼 台湾 01", false), format!("自动 · 在用 {}", sanitize_name("🇹🇼 台湾 01", SC_NAME_MAX)));
    assert!(auto_row_label("TW04", "示例节点 03", false).contains("首选不通"));
    assert_eq!(auto_row_label("TW04", "TW04", false), "自动");

    let mut c = Chill::default();
    c.parse_all_array(br#"["TW04","TW05"]"#);
    c.fixed = Some("TW04".into());
    c.nodes.insert(0, "自动".into());
    c.nodes_raw.insert(0, String::new());
    c.node_delay.insert(0, -1);
    c.node = "TW05".into();
    let sel: Vec<i32> = c.nodes().iter().map(|n| n.selected).collect();
    assert_eq!(sel, vec![0, 1, 0]);
    c.fixed = Some(String::new());
    let sel: Vec<i32> = c.nodes().iter().map(|n| n.selected).collect();
    assert_eq!(sel, vec![1, 0, 0]);
}

#[test]
fn read_snapshot_does_not_share_or_replace_the_live_delay_socket() {
    use crate::data::background::ReadCopy;
    use std::os::fd::{AsRawFd, OwnedFd};
    let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let fd: OwnedFd = socket.into();
    let raw = fd.as_raw_fd();
    let mut chill = Chill {
        delay_fd: Some(fd), delay_buf: b"pending response".to_vec(), delay_t0: 123,
        ..Default::default()
    };
    let mut next = chill.read_copy();
    assert!(next.delay_fd.is_none() && next.delay_buf.is_empty());
    next.online = true;
    chill.apply(next);
    assert!(chill.online);
    assert_eq!(chill.delay_fd.as_ref().unwrap().as_raw_fd(), raw);
    assert_eq!(chill.delay_buf, b"pending response");
    assert_eq!(chill.delay_t0, 123);
}

// ---- Home's pinned line ----

/// The shape of mihomo's /group on 2026-10-29: 🤖 AI → a region group → a node; 📞 VoWiFi
/// straight on a node; "now" after "name" in each object.
const PIN_GROUPS: &str = r#"{"proxies":[{"all":["🇹🇼 台湾","🇯🇵 日本"],"history":[],"name":"🤖 AI","now":"🇹🇼 台湾","type":"Selector"},{"all":["🇹🇼 示例节点 02丨3x TW"],"history":[],"name":"🇹🇼 台湾","now":"🇹🇼 示例节点 02丨3x TW","type":"URLTest"},{"all":["🇹🇼 示例节点 03丨3x TW"],"name":"📞 VoWiFi","now":"🇹🇼 示例节点 03丨3x TW","type":"Selector"},{"name":"🍎 Apple","now":"DIRECT","type":"Selector"}]}"#;

#[test]
fn pin_chain_follows_groups_to_the_node() {
    let now = group_nows(PIN_GROUPS.as_bytes());
    assert_eq!(pin_chain(&now, "🤖 AI"), Some(("🇹🇼 示例节点 02丨3x TW".into(), "TW".into())));
    assert_eq!(pin_chain(&now, "📞 VoWiFi"), Some(("🇹🇼 示例节点 03丨3x TW".into(), "TW".into())));
    assert_eq!(pin_chain(&now, "🍎 Apple"), Some(("DIRECT".into(), "DIRECT".into())));
    assert_eq!(pin_chain(&now, "📺 流媒体"), None, "a group that is not there");
}

#[test]
fn flag_of_takes_the_first_flag() {
    assert_eq!(flag_of("🇯🇵 Japan 25"), "JP");
    assert_eq!(flag_of("香港 01"), "");
    assert_eq!(flag_of("🚀 节点选择"), "");
}
