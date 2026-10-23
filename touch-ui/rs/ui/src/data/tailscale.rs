//! Tailscale for Home's card, its 出口 line and the Tailscale page. The source is tailscaled's
//! own LocalAPI, `GET /localapi/v0/status` over its unix socket: about 2 ms, where the CLI takes
//! seconds. Read every 5 s while one of those is on screen; which socket exists is checked on
//! every read, because at boot the UI is usually up before tailscaled is.

use super::http;
use super::util::{jget, jtrue, mono_ms, objects, text, trunc, wall};
use crate::json;
use ui_model::{tailscale_peer_t, tailscale_status_t};
use std::os::fd::AsRawFd;

/// tailscaled answers in a couple of ms; one that hangs must not hold Home up for long
const TS_IO_MS: i32 = 500;
const TS_TTL_MS: i64 = 5000;
/// a socket that exists but did not answer is asked less often
const TS_BACKOFF_MS: i64 = 15000;
const TS_RESP_MAX: usize = 131072;
pub const TS_PEER_MAX: usize = 12;
const SOCKS: [&str; 2] = ["/tmp/tailscaled.sock", "/var/run/tailscale/tailscaled.sock"];

#[derive(Default)]
#[derive(Clone)]
pub struct Tailscale {
    sock: Option<&'static str>,
    pub st: tailscale_status_t,
    peers: Vec<tailscale_peer_t>,
    was_active: bool,
    poll_ms: i64,
    sig: Option<String>,
}

/// What a node is called on screen: the host part of its MagicDNS name
/// ("u60-pro.tail0.ts.net." gives "u60-pro"); a node without one goes by HostName.
fn node_name(node: &[u8], cap: usize) -> String {
    match json::get(node, "DNSName", 256).filter(|d| !d.is_empty()) {
        Some(dns) => {
            let host = dns.split(|&c| c == b'.').next().unwrap_or_default();
            trunc(&text(host), cap)
        }
        None => jget(node, "HostName", cap),
    }
}

/// The inside of every "…" in `arr`, in order and as written (escapes stay escapes). A string
/// that is not closed, or runs into a NUL, ends the walk.
fn quoted(arr: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = arr;
    std::iter::from_fn(move || {
        let open = rest.iter().position(|&c| c == b'"')?;
        let body = &rest[open + 1..];
        let mut i = 0;
        loop {
            match body.get(i) {
                None | Some(0) => return None,
                Some(b'"') => break,
                Some(b'\\') if i + 1 < body.len() => i += 2,
                Some(_) => i += 1,
            }
        }
        rest = &body[i + 1..];
        Some(&body[..i])
    })
}

/// Up to `max` strings of a JSON array, `sep` between them, in a buffer of `cap` (so at most
/// cap - 1 bytes). The string that would overflow is cut at the edge and nothing follows it.
fn list_text(arr: &[u8], sep: &str, max: usize, cap: usize) -> String {
    let room = cap - 1;
    let mut out = Vec::with_capacity(room);
    for (n, s) in quoted(arr).take(max).enumerate() {
        let gap: &[u8] = if n == 0 { b"" } else { sep.as_bytes() };
        let whole = gap.len() + s.len();
        let left = room - out.len();
        out.extend(gap.iter().chain(s).take(left));
        if whole > left {
            break;
        }
    }
    text(&out)
}

/// "2026-09-25T03:04:05.123Z" → seconds before now; the zero time (0001-…) or unreadable → -1.
/// tailscaled writes UTC from the system clock; the device clock is local time labelled UTC:
/// the same base, so the difference is right.
fn iso_ago(iso: &str, now: i64) -> i64 {
    let b = iso.as_bytes();
    let num = |a: usize, n: usize| -> Option<i64> {
        let s = b.get(a..a + n)?;
        s.iter().all(u8::is_ascii_digit).then(|| json::atol(s))
    };
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(s)) = (num(0, 4), num(5, 2), num(8, 2), num(11, 2), num(14, 2), num(17, 2)) else {
        return -1;
    };
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') || b.get(10) != Some(&b'T') || y < 2000 {
        return -1;
    }
    // timegm: days from the civil date
    let (y2, m2) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let t = days * 86400 + h * 3600 + mi * 60 + s;
    if t > now { 0 } else { now - t }
}

impl Tailscale {
    /// One LocalAPI GET; the body only when tailscaled said 200.
    fn ask(sock: &str, path: &str) -> Option<Vec<u8>> {
        let req = http::build("GET", path, "local-tailscaled.sock", "", None, 256)?;
        let conn = http::connect_unix(sock)?;
        if http::wait(conn.as_raw_fd(), true, TS_IO_MS) <= 0 {
            return None;
        }
        let resp = http::exchange(conn, req.as_bytes(), TS_RESP_MAX, TS_IO_MS);
        (resp.status == 200).then_some(resp.body).flatten()
    }

    /// The status answer into what the card and the page show.
    pub fn parse_status(&mut self, b: &[u8]) {
        self.st = tailscale_status_t { available: self.st.available, ok: self.st.ok, ..self.read_me(b) };
        self.read_peers(b);
    }

    /// The tailnet-wide fields and this node's own.
    fn read_me(&self, b: &[u8]) -> tailscale_status_t {
        let mut st = tailscale_status_t {
            state: jget(b, "BackendState", 24),
            health: json::get(b, "Health", 1024).map(|h| list_text(&h, "", 1, 160)).unwrap_or_default(),
            ..Default::default()
        };
        if let Some(v) = json::get(b, "Version", 24) {
            // "1.102.4-t0123abcd-g4567": the release number is enough
            st.version = text(v.split(|&c| c == b'-').next().unwrap_or_default());
        }
        if let Some(net) = json::get(b, "CurrentTailnet", 256).filter(|n| n.starts_with(b"{")) {
            st.tailnet = jget(&net, "Name", 64);
        }
        let Some(me) = json::get(b, "Self", 16384) else { return st };
        st.name = node_name(&me, 64);
        if let Some(ips) = json::get(&me, "TailscaleIPs", 1024) {
            st.ip = list_text(&ips, "", 1, 48);
            // the v6 address is the second one; both are read through the same 128-byte buffer
            let both = list_text(&ips, ",", 2, 128);
            if let Some((_, v6)) = both.split_once(',') {
                st.ip6 = trunc(v6, 48);
            }
        }
        st.os = jget(&me, "OS", 16);
        // the zero time (0001-…) is a key that does not expire
        if let Some(exp) = json::get(&me, "KeyExpiry", 40).filter(|e| e.len() >= 10 && !e.starts_with(b"0001")) {
            st.key_expiry = text(&exp[..10]);
        }
        st.relay = jget(&me, "Relay", 32);
        if let Some(routes) = json::get(&me, "PrimaryRoutes", 1024) {
            st.routes = list_text(&routes, ", ", 4, 160);
        }
        st.self_online = jtrue(&me, "Online") as i32;
        st
    }

    /// "Peer" maps node keys to node objects: count them, keep the first TS_PEER_MAX for the
    /// page, and note which one (if any) is the exit node.
    fn read_peers(&mut self, b: &[u8]) {
        self.peers.clear();
        let Some(map) = json::get(b, "Peer", TS_RESP_MAX).filter(|m| m.starts_with(b"{")) else { return };
        let now = wall();
        for node in objects(&map[1..]) {
            let online = jtrue(node, "Online");
            let active = jtrue(node, "Active");
            // a peer talking to us straight has CurAddr; one going through DERP has it empty
            let direct = active && json::get(node, "CurAddr", 128).is_some_and(|a| !a.is_empty());
            let exit = jtrue(node, "ExitNode");
            let st = &mut self.st;
            st.peers += 1;
            st.peers_online += online as i32;
            st.active += active as i32;
            st.direct += direct as i32;
            if exit {
                st.exit_node = node_name(node, 64);
                st.exit_online = online as i32;
            }
            if self.peers.len() == TS_PEER_MAX {
                continue;
            }
            let since = |key: &str| json::get(node, key, 48).map_or(-1, |t| iso_ago(&text(&t), now));
            self.peers.push(tailscale_peer_t {
                name: node_name(node, 40),
                ip: json::get(node, "TailscaleIPs", 256).map(|a| list_text(&a, "", 1, 24)).unwrap_or_default(),
                online: online as i32,
                active: active as i32,
                direct: direct as i32,
                exit_node: exit as i32,
                os: jget(node, "OS", 16),
                cur_addr: jget(node, "CurAddr", 48),
                relay: jget(node, "Relay", 16),
                rx: json::get_int(node, "RxBytes", 0),
                tx: json::get_int(node, "TxBytes", 0),
                hs_ago: since("LastHandshake"),
                seen_ago: since("LastSeen"),
            });
        }
    }

    /// What the card and the page compare to know whether to redraw.
    fn shown(&self) -> String {
        let st = &self.st;
        let counts = [st.available, st.ok, st.self_online, st.exit_online, st.peers, st.peers_online, st.active, st.direct];
        let mut s = counts.map(|n| n.to_string()).join("/");
        for field in [&st.state, &st.name, &st.ip, &st.relay, &st.routes, &st.exit_node, &st.health] {
            s.push('\u{1}');
            s.push_str(field);
        }
        s
    }

    /// active: Home, 出口 or the page is on a lit screen. true when what they show changed.
    pub fn poll(&mut self, active: bool) -> bool {
        if !active {
            self.was_active = false;
            return false;
        }
        let now = mono_ms();
        let unanswered = self.sock.is_some() && self.st.ok == 0;
        let every = if unanswered { TS_BACKOFF_MS } else { TS_TTL_MS };
        if self.was_active && now - self.poll_ms < every {
            return false;
        }
        self.was_active = true;
        self.poll_ms = now;
        self.sock = SOCKS.into_iter().find(|s| std::path::Path::new(s).exists());
        let body = self.sock.and_then(|s| Self::ask(s, "/localapi/v0/status"));
        self.st.ok = 0;
        if let Some(b) = body {
            self.parse_status(&b);
            self.st.ok = 1;
        }
        self.st.available = self.sock.is_some() as i32;
        let sig = self.shown();
        if self.sig.as_ref() == Some(&sig) {
            return false;
        }
        self.sig = Some(sig);
        true
    }

    pub fn status(&self) -> tailscale_status_t {
        tailscale_status_t { available: self.sock.is_some() as i32, ..self.st.clone() }
    }

    pub fn peers(&self) -> &[tailscale_peer_t] {
        &self.peers
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn iso(ago: i64) -> String {
        // the inverse of iso_ago, for the test
        let t = wall() - ago;
        let days = t.div_euclid(86400);
        let secs = t.rem_euclid(86400);
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + (m <= 2) as i64;
        format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.123456789Z", secs / 3600, secs / 60 % 60, secs % 60)
    }

    #[test]
    fn status_fields() {
        let (hs, seen) = (iso(42), iso(3 * 3600));
        let body = format!(
            r#"{{"Version":"1.102.4-t0123abcd-g4567","BackendState":"Running","Health":[],"CurrentTailnet":{{"Name":"me@example.com","MagicDNSSuffix":"tail0.ts.net"}},"Self":{{"DNSName":"u60-pro.tail0.ts.net.","HostName":"u60","OS":"linux","TailscaleIPs":["100.64.0.7","fd7a:115c:a1e0::7"],"Relay":"hkg","Online":true,"PrimaryRoutes":["192.168.0.0/24"],"KeyExpiry":"2027-03-01T08:00:00Z"}},"Peer":{{"nodekey:a":{{"DNSName":"macbook-pro.tail0.ts.net.","OS":"macOS","TailscaleIPs":["100.64.0.2"],"Relay":"tok","CurAddr":"203.0.113.5:41641","RxBytes":1234567890,"TxBytes":5555,"Online":true,"Active":true,"ExitNode":false,"LastHandshake":"{hs}","LastSeen":"0001-01-01T00:00:00Z"}},"nodekey:b":{{"DNSName":"iphone.tail0.ts.net.","OS":"iOS","TailscaleIPs":["100.64.0.3"],"Relay":"hkg","CurAddr":"","RxBytes":0,"TxBytes":0,"Online":false,"Active":false,"LastHandshake":"0001-01-01T00:00:00Z","LastSeen":"{seen}"}}}}}}"#
        );
        let mut t = Tailscale::default();
        t.parse_status(body.as_bytes());
        let st = &t.st;
        assert!(st.name == "u60-pro" && st.ip == "100.64.0.7");
        assert_eq!(st.ip6, "fd7a:115c:a1e0::7");
        assert_eq!(st.version, "1.102.4");
        assert_eq!(st.tailnet, "me@example.com");
        assert_eq!(st.key_expiry, "2027-03-01");
        assert_eq!(st.os, "linux");
        assert!(t.peers().len() == 2 && st.peers == 2 && st.direct == 1);
        let pe = &t.peers()[0];
        assert!(pe.os == "macOS" && pe.cur_addr == "203.0.113.5:41641" && pe.relay == "tok");
        assert!(pe.rx == 1234567890 && pe.tx == 5555);
        assert!((41..=45).contains(&pe.hs_ago), "{}", pe.hs_ago);
        assert_eq!(pe.seen_ago, -1, "the zero LastSeen is unknown");
        let pe = &t.peers()[1];
        assert_eq!(pe.hs_ago, -1, "never handshaked");
        assert!((3 * 3600 - 2..=3 * 3600 + 5).contains(&pe.seen_ago));

        t.parse_status(br#"{"BackendState":"NeedsLogin","Self":{"KeyExpiry":"0001-01-01T00:00:00Z"},"Peer":null}"#);
        assert!(t.st.key_expiry.is_empty() && t.st.version.is_empty() && t.peers().is_empty());
    }
}
