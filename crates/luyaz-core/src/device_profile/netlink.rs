//! Connection epoch producer (R18-P1-01, firmware-capabilities §12.6): the kernel's
//! rtnetlink stream for the cellular data interfaces (`rmnet*`).
//!
//! Contract: subscribe first, then dump links; the epoch exists only after the dump's
//! `NLMSG_DONE`. Every rmnet link/address/route event mints a new opaque epoch, repeats
//! included (a same-IP redial is a delete + add of the same address: ABA). The kernel
//! reports a dropped multicast message as `ENOBUFS` (we never set `NETLINK_NO_ENOBUFS`);
//! that, any other read error, a truncated or malformed message, or the socket closing
//! revokes the epoch until a new subscription completes its dump. No timer, no polling:
//! the thread sleeps in `recv` and only wakes for kernel messages.
//!
//! Measured on CN B31 once (same-IP IPv4 redial). SSR, SIM swap, IPv6-only loss and a
//! new-IP redial were not observed; the epoch is not claimed to cover them beyond the
//! kernel events they cause.

use std::collections::BTreeSet;

pub(crate) const RTM_NEWLINK: u16 = 16;
pub(crate) const RTM_DELLINK: u16 = 17;
pub(crate) const RTM_NEWADDR: u16 = 20;
pub(crate) const RTM_DELADDR: u16 = 21;
pub(crate) const RTM_NEWROUTE: u16 = 24;
pub(crate) const RTM_DELROUTE: u16 = 25;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLMSG_OVERRUN: u16 = 4;
const IFLA_IFNAME: u16 = 3;
const RTA_OIF: u16 = 4;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Msg {
    Link {
        index: u32,
        name: Option<String>,
        /// `ifi_flags` (UP, LOWER_UP, RUNNING…).
        flags: u32,
        gone: bool,
    },
    Addr {
        index: u32,
        /// RTM_DELADDR of a scope-universe address: a drop for `radio.band_flap`.
        gone_global: bool,
    },
    Route {
        oif: Option<u32>,
    },
    Done,
    /// `NLMSG_ERROR` with a non-zero code, or `NLMSG_OVERRUN`.
    Failed,
    Other,
}

/// Malformed input is an error: the caller revokes rather than guesses.
pub(crate) fn parse(mut buf: &[u8]) -> Result<Vec<Msg>, ()> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        if buf.len() < 16 {
            return Err(());
        }
        let len = u32::from_ne_bytes(buf[0..4].try_into().unwrap()) as usize;
        let kind = u16::from_ne_bytes(buf[4..6].try_into().unwrap());
        if len < 16 || len > buf.len() {
            return Err(());
        }
        let body = &buf[16..len];
        out.push(match kind {
            RTM_NEWLINK | RTM_DELLINK => {
                // ifinfomsg: family, pad, type u16, index i32, flags u32, change u32
                if body.len() < 16 {
                    return Err(());
                }
                let index = u32::from_ne_bytes(body[4..8].try_into().unwrap());
                let flags = u32::from_ne_bytes(body[8..12].try_into().unwrap());
                let name = attr(&body[16..], IFLA_IFNAME)?.map(|v| {
                    let v = v.split(|&b| b == 0).next().unwrap_or(&[]);
                    String::from_utf8_lossy(v).into_owned()
                });
                Msg::Link {
                    index,
                    name,
                    flags,
                    gone: kind == RTM_DELLINK,
                }
            }
            RTM_NEWADDR | RTM_DELADDR => {
                // ifaddrmsg: family, prefixlen, flags, scope, index u32
                if body.len() < 8 {
                    return Err(());
                }
                Msg::Addr {
                    index: u32::from_ne_bytes(body[4..8].try_into().unwrap()),
                    gone_global: kind == RTM_DELADDR && body[3] == 0,
                }
            }
            RTM_NEWROUTE | RTM_DELROUTE => {
                // rtmsg is 12 bytes
                if body.len() < 12 {
                    return Err(());
                }
                let oif = attr(&body[12..], RTA_OIF)?
                    .and_then(|v| v.get(..4))
                    .map(|v| u32::from_ne_bytes(v.try_into().unwrap()));
                Msg::Route { oif }
            }
            NLMSG_DONE => Msg::Done,
            NLMSG_ERROR => {
                let code = body
                    .get(..4)
                    .map(|v| i32::from_ne_bytes(v.try_into().unwrap()))
                    .ok_or(())?;
                if code == 0 { Msg::Other } else { Msg::Failed }
            }
            NLMSG_OVERRUN => Msg::Failed,
            _ => Msg::Other,
        });
        buf = &buf[align(len).min(buf.len())..];
    }
    Ok(out)
}

fn align(n: usize) -> usize {
    (n + 3) & !3
}

fn attr(mut buf: &[u8], want: u16) -> Result<Option<&[u8]>, ()> {
    while buf.len() >= 4 {
        let len = u16::from_ne_bytes(buf[0..2].try_into().unwrap()) as usize;
        let kind = u16::from_ne_bytes(buf[2..4].try_into().unwrap()) & 0x3fff;
        if len < 4 || len > buf.len() {
            return Err(());
        }
        if kind == want {
            return Ok(Some(&buf[4..len]));
        }
        buf = &buf[align(len).min(buf.len())..];
    }
    Ok(None)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Nothing,
    /// Dump finished on a subscribed socket: the stream is complete from here on.
    Live,
    /// A data-interface change: mint a new epoch.
    Bump,
    /// A global address left a data interface: mint a new epoch and count a drop.
    Drop,
    /// Revoke and resubscribe.
    Lost,
}

/// Pure event → epoch logic for one subscription.
#[derive(Default)]
pub(crate) struct Tracker {
    live: bool,
    /// Every link index seen; an address/route on an index we never saw counts as ours.
    known: BTreeSet<u32>,
    rmnet: BTreeSet<u32>,
    /// Last flags per rmnet link: the IPA driver repeats an unchanged RTM_NEWLINK about
    /// five times a second on the device (10-20), which would remint the epoch nonstop.
    flags: std::collections::BTreeMap<u32, u32>,
}

impl Tracker {
    pub(crate) fn on(&mut self, msg: &Msg) -> Step {
        let ours = match msg {
            Msg::Failed => return Step::Lost,
            Msg::Done => {
                if self.live {
                    return Step::Nothing;
                }
                self.live = true;
                return Step::Live;
            }
            Msg::Other => return Step::Nothing,
            Msg::Link {
                index,
                name,
                flags,
                gone,
            } => {
                // The same rmnet link with the same flags again: no state change. A down/up,
                // a rename, a new or deleted link, and every address/route event still bump.
                let repeat = !*gone
                    && self.rmnet.contains(index)
                    && name.as_deref().is_none_or(|n| n.starts_with("rmnet"))
                    && self.flags.get(index) == Some(flags);
                let ours = match name {
                    Some(n) => n.starts_with("rmnet"),
                    None => !self.known.contains(index) || self.rmnet.contains(index),
                };
                if *gone {
                    self.known.remove(index);
                    self.rmnet.remove(index);
                    self.flags.remove(index);
                } else {
                    self.known.insert(*index);
                    if name.as_deref().is_some_and(|n| n.starts_with("rmnet")) {
                        self.rmnet.insert(*index);
                    } else if name.is_some() {
                        self.rmnet.remove(index);
                    }
                    if self.rmnet.contains(index) {
                        self.flags.insert(*index, *flags);
                    } else {
                        self.flags.remove(index);
                    }
                }
                if repeat {
                    return Step::Nothing;
                }
                ours || self.rmnet.contains(index)
            }
            Msg::Addr {
                index,
                gone_global: true,
            } if self.live && (!self.known.contains(index) || self.rmnet.contains(index)) => {
                return Step::Drop;
            }
            Msg::Addr { index, .. } | Msg::Route { oif: Some(index) } => {
                !self.known.contains(index) || self.rmnet.contains(index)
            }
            // A route without an output interface: can't rule it out.
            Msg::Route { oif: None } => true,
        };
        // Before the dump completes nothing is minted; the mint at Done covers it.
        if ours && self.live {
            Step::Bump
        } else {
            Step::Nothing
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn spawn(hub: std::sync::Arc<crate::block::Hub>) {
    let _ = std::thread::Builder::new()
        .name("luyaz-netlink".into())
        .spawn(move || {
            let mut backoff = 5u64;
            loop {
                let reached_live = run(&hub);
                hub.firmware().connection_live(false);
                if reached_live {
                    backoff = 5;
                }
                eprintln!("luyaz-core: rtnetlink 连接代次已撤销，{backoff}s 后重新订阅");
                std::thread::sleep(std::time::Duration::from_secs(backoff));
                backoff = (backoff * 2).min(300);
            }
        });
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn spawn(_hub: std::sync::Arc<crate::block::Hub>) {}

/// One subscription until it is lost. Returns whether it ever went live.
#[cfg(target_os = "linux")]
fn run(hub: &crate::block::Hub) -> bool {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    // SAFETY: plain socket syscalls on a descriptor we own; buffers outlive each call.
    unsafe {
        let fd = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            libc::NETLINK_ROUTE,
        );
        if fd < 0 {
            return false;
        }
        let fd = OwnedFd::from_raw_fd(fd);
        let mut sa: libc::sockaddr_nl = std::mem::zeroed();
        sa.nl_family = libc::AF_NETLINK as u16;
        sa.nl_groups = (libc::RTMGRP_LINK
            | libc::RTMGRP_IPV4_IFADDR
            | libc::RTMGRP_IPV6_IFADDR
            | libc::RTMGRP_IPV4_ROUTE
            | libc::RTMGRP_IPV6_ROUTE) as u32;
        let sl = std::mem::size_of::<libc::sockaddr_nl>() as u32;
        if libc::bind(fd.as_raw_fd(), &sa as *const _ as *const libc::sockaddr, sl) < 0 {
            return false;
        }
        // Subscribed; now the dump (nlmsghdr + rtgenmsg, AF_UNSPEC).
        let mut req = [0u8; 20];
        req[0..4].copy_from_slice(&20u32.to_ne_bytes());
        req[4..6].copy_from_slice(&RTM_GETLINK.to_ne_bytes());
        let flags = (libc::NLM_F_REQUEST | libc::NLM_F_DUMP) as u16;
        req[6..8].copy_from_slice(&flags.to_ne_bytes());
        req[8..12].copy_from_slice(&1u32.to_ne_bytes());
        if libc::send(fd.as_raw_fd(), req.as_ptr().cast(), req.len(), 0) != req.len() as isize {
            return false;
        }
        let mut tracker = Tracker::default();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = libc::recv(
                fd.as_raw_fd(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                libc::MSG_TRUNC,
            );
            if n == -1 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            // ENOBUFS (lost multicast), any error, EOF or truncation: revoke.
            if n <= 0 || n as usize > buf.len() {
                return tracker.live;
            }
            let Ok(msgs) = parse(&buf[..n as usize]) else {
                return tracker.live;
            };
            for msg in &msgs {
                match tracker.on(msg) {
                    Step::Nothing => {}
                    Step::Live => hub.firmware().connection_live(true),
                    Step::Bump => hub.firmware().connection_event(),
                    Step::Drop => {
                        hub.firmware().connection_event();
                        crate::api::band_flap::drop_seen(hub);
                    }
                    Step::Lost => return tracker.live,
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
const RTM_GETLINK: u16 = 18;

#[cfg(test)]
#[path = "netlink_tests.rs"]
mod tests;
