//! Home's Tailscale card and page (src/tailscale.c): tailscaled's LocalAPI
//! (GET /localapi/v0/status on its unix socket, ~2 ms; not the CLI), every 5 s while Home,
//! 出口 or the page is up; the socket probed each time (the UI often starts before tailscaled).
// SPDX-License-Identifier: MIT

use super::http;
use super::util::{find_byte, jget, mono_ms, obj_end, text, trunc, wall};
use crate::json;
use devui_model::{tailscale_peer_t, tailscale_status_t};
use std::os::fd::AsRawFd;

/// the local socket answers in 2 ms; a stuck tailscaled must not hold Home up
const TS_IO_MS: i32 = 500;
const TS_TTL_MS: i64 = 5000;
/// the socket is there but does not answer
const TS_BACKOFF_MS: i64 = 15000;
const TS_RESP_MAX: usize = 131072;
pub const TS_PEER_MAX: usize = 12;
const SOCKS: [&str; 2] = ["/tmp/tailscaled.sock", "/var/run/tailscale/tailscaled.sock"];

#[derive(Default)]
pub struct Tailscale {
    sock: Option<&'static str>,
    pub st: tailscale_status_t,
    peers: Vec<tailscale_peer_t>,
    was_active: bool,
    poll_ms: i64,
    sig: Option<String>,
}

/// DNSName's first label ("u60-pro.tail0.ts.net." → u60-pro), else HostName, at most cap - 1.
fn short_name(o: &[u8], cap: usize) -> String {
    match json::get(o, "DNSName", 256) {
        Some(d) if !d.is_empty() => {
            let d = text(&d);
            trunc(d.split('.').next().unwrap_or(""), cap)
        }
        _ => jget(o, "HostName", cap),
    }
}

/// The first `max` strings of an array joined with sep (as they are, escapes not decoded); a
/// piece that does not fit is cut and ends the list, like the C snprintf.
fn join_strings(arr: &[u8], sep: &str, max: usize, cap: usize) -> String {
    let mut out: Vec<u8> = Vec::new();
    let mut p = 0;
    for n in 0..max {
        let Some(q) = find_byte(arr, b'"', p) else { break };
        let mut e = q + 1;
        while e < arr.len() && arr[e] != 0 && arr[e] != b'"' {
            e += if arr[e] == b'\\' && e + 1 < arr.len() { 2 } else { 1 };
        }
        if e >= arr.len() || arr[e] != b'"' {
            break;
        }
        let mut piece = Vec::new();
        if n > 0 {
            piece.extend_from_slice(sep.as_bytes());
        }
        piece.extend_from_slice(&arr[q + 1..e]);
        let room = cap - 1 - out.len();
        if piece.len() > room {
            out.extend_from_slice(&piece[..room]);
            break;
        }
        out.extend_from_slice(&piece);
        p = e + 1;
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
    fn get(sock: &str, path: &str) -> Option<Vec<u8>> {
        let req = http::build("GET", path, "local-tailscaled.sock", "", None, 256)?;
        let fd = http::connect_unix(sock)?;
        if http::wait(fd.as_raw_fd(), true, TS_IO_MS) <= 0 {
            return None;
        }
        let r = http::exchange(fd, req.as_bytes(), TS_RESP_MAX, TS_IO_MS);
        if r.status == 200 { r.body } else { None }
    }

    /// /localapi/v0/status into the fields the card and the page show (parse_status).
    pub fn parse_status(&mut self, b: &[u8]) {
        let st = &mut self.st;
        st.state = jget(b, "BackendState", 24);
        st.health = json::get(b, "Health", 1024).map(|a| join_strings(&a, "", 1, 160)).unwrap_or_default();
        st.name.clear();
        st.ip.clear();
        st.relay.clear();
        st.routes.clear();
        st.ip6.clear();
        st.os.clear();
        st.version.clear();
        st.tailnet.clear();
        st.key_expiry.clear();
        st.self_online = 0;
        if let Some(v) = json::get(b, "Version", 24) {
            st.version = text(&v).split('-').next().unwrap_or("").to_string();
        }
        if let Some(tn) = json::get(b, "CurrentTailnet", 256)
            && tn.first() == Some(&b'{')
        {
            st.tailnet = jget(&tn, "Name", 64);
        }
        if let Some(me) = json::get(b, "Self", 16384) {
            st.name = short_name(&me, 64);
            if let Some(arr) = json::get(&me, "TailscaleIPs", 1024) {
                st.ip = join_strings(&arr, "", 1, 48);
                let two = join_strings(&arr, ",", 2, 128);
                if let Some(c) = two.find(',') {
                    st.ip6 = trunc(&two[c + 1..], 48);
                }
            }
            st.os = jget(&me, "OS", 16);
            if let Some(ke) = json::get(&me, "KeyExpiry", 40)
                && ke.len() >= 10
                && !ke.starts_with(b"0001")
            {
                st.key_expiry = text(&ke[..10]);
            }
            st.relay = jget(&me, "Relay", 32);
            if let Some(arr) = json::get(&me, "PrimaryRoutes", 1024) {
                st.routes = join_strings(&arr, ", ", 4, 160);
            }
            st.self_online = (json::get(&me, "Online", 8).is_some_and(|v| v == b"true")) as i32;
        }

        // Peer is {"nodekey:…": {…}, …}: each object cut out on its own
        st.peers = 0;
        st.peers_online = 0;
        st.active = 0;
        st.direct = 0;
        st.exit_node.clear();
        st.exit_online = 0;
        self.peers.clear();
        if let Some(peer) = json::get(b, "Peer", TS_RESP_MAX)
            && peer.first() == Some(&b'{')
        {
            let now = wall();
            let mut p = 1;
            while let Some(o) = find_byte(&peer, b'{', p) {
                let Some(e) = obj_end(&peer, o) else { break };
                let pb = &peer[o..=e];
                let tru = |k: &str| json::get(pb, k, 8).is_some_and(|v| v == b"true");
                st.peers += 1;
                let (online, active) = (tru("Online"), tru("Active"));
                let mut direct = false;
                if online {
                    st.peers_online += 1;
                }
                if active {
                    st.active += 1;
                    // CurAddr set = hole-punched; empty = through DERP
                    if json::get(pb, "CurAddr", 128).is_some_and(|c| !c.is_empty()) {
                        st.direct += 1;
                        direct = true;
                    }
                }
                if self.peers.len() < TS_PEER_MAX {
                    let ago = |k: &str| json::get(pb, k, 48).map_or(-1, |t| iso_ago(&text(&t), now));
                    self.peers.push(tailscale_peer_t {
                        name: short_name(pb, 40),
                        ip: json::get(pb, "TailscaleIPs", 256).map(|a| join_strings(&a, "", 1, 24)).unwrap_or_default(),
                        online: online as i32,
                        active: active as i32,
                        direct: direct as i32,
                        exit_node: tru("ExitNode") as i32,
                        os: jget(pb, "OS", 16),
                        cur_addr: jget(pb, "CurAddr", 48),
                        relay: jget(pb, "Relay", 16),
                        rx: json::get_int(pb, "RxBytes", 0),
                        tx: json::get_int(pb, "TxBytes", 0),
                        hs_ago: ago("LastHandshake"),
                        seen_ago: ago("LastSeen"),
                    });
                }
                if tru("ExitNode") {
                    st.exit_node = short_name(pb, 64);
                    st.exit_online = online as i32;
                }
                p = e + 1;
            }
        }
    }

    /// active = Home, 出口 or the page is up on a lit screen. true = what shows changed.
    pub fn poll(&mut self, active: bool) -> bool {
        if !active {
            self.was_active = false;
            return false;
        }
        let t = mono_ms();
        let ttl = if self.sock.is_some() && self.st.ok == 0 { TS_BACKOFF_MS } else { TS_TTL_MS };
        if self.was_active && t - self.poll_ms < ttl {
            return false;
        }
        self.was_active = true;
        self.poll_ms = t;
        // probed every time: at boot the UI is often up before tailscaled
        self.sock = SOCKS.iter().copied().find(|s| std::path::Path::new(s).exists());
        self.st.ok = 0;
        if let Some(s) = self.sock
            && let Some(b) = Self::get(s, "/localapi/v0/status")
        {
            self.parse_status(&b);
            self.st.ok = 1;
        }
        self.st.available = self.sock.is_some() as i32;
        let st = &self.st;
        let sig = format!(
            "{}/{}/{}/{}/{}/{}/{}/{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}",
            st.available,
            st.ok,
            st.self_online,
            st.exit_online,
            st.peers,
            st.peers_online,
            st.active,
            st.direct,
            st.state,
            st.name,
            st.ip,
            st.relay,
            st.routes,
            st.exit_node,
            st.health
        );
        if self.sig.as_ref() == Some(&sig) {
            return false;
        }
        self.sig = Some(sig);
        true
    }

    pub fn status(&self) -> tailscale_status_t {
        let mut s = self.st.clone();
        s.available = self.sock.is_some() as i32;
        s
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

    // tests/tailscale_test.c
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
