//! USB epoch producer (firmware-capabilities §12.10): the kernel's kobject uevent stream.
//!
//! Contract: bind a `NETLINK_KOBJECT_UEVENT` socket (group 1) first, then read every
//! `/sys/class/udc/*/state` once as the snapshot; the epoch exists only after that. Every
//! USB-related uevent (subsystem udc/android_usb/usb/typec/power_supply, or a `USB_STATE=`
//! key) mints a new opaque epoch, repeats included. `SEQNUM` is checked across *all* kernel
//! uevents, related or not: a gap, a repeat or a step backwards means a message was lost
//! (or the stream can't be trusted), as do `ENOBUFS`, any other read error, a truncated,
//! oversized or malformed message and the socket closing. Each of those revokes the epoch
//! until a new socket has taken a new snapshot. libudev re-broadcasts (group 2, "libudev"
//! magic) are not ours and are ignored. No timer, no polling: the thread sleeps in `recv`.
//!
//! Measured on CN B31 once (10-24, one unplug/replug): UDC state configured -> "not
//! attached" -> configured, about 33 uevents over 5 s on unplug and 44 on replug, about one
//! uevent a minute while idle. The subsystems of those uevents were not captured, hence the
//! broad filter. Not authoritative.

/// The kernel's uevent buffer is 2 KiB; anything far larger is not a kernel uevent.
pub(crate) const MAX_MSG: usize = 8 * 1024;

const SUBSYSTEMS: [&str; 5] = ["udc", "android_usb", "usb", "typec", "power_supply"];

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Msg {
    /// A kernel uevent (`action@devpath\0KEY=VALUE\0…`).
    Kernel { seqnum: u64, relevant: bool },
    /// A libudev re-broadcast: not part of the kernel sequence.
    Libudev,
}

/// Malformed input is an error: the caller revokes rather than guesses.
pub(crate) fn parse(buf: &[u8]) -> Result<Msg, ()> {
    if buf.starts_with(b"libudev\0") {
        return Ok(Msg::Libudev);
    }
    // Kernel messages end with a NUL; without one the read was cut short.
    if buf.is_empty() || buf.len() > MAX_MSG || buf.last() != Some(&0) {
        return Err(());
    }
    let mut fields = buf[..buf.len() - 1].split(|&b| b == 0);
    let header = fields.next().ok_or(())?;
    if !header.contains(&b'@') {
        return Err(());
    }
    let (mut seqnum, mut relevant) = (None, false);
    for field in fields {
        let field = String::from_utf8_lossy(field);
        let Some((key, value)) = field.split_once('=') else {
            return Err(());
        };
        match key {
            "SEQNUM" => {
                if seqnum.is_some() {
                    return Err(());
                }
                seqnum = Some(value.parse::<u64>().map_err(|_| ())?);
            }
            "SUBSYSTEM" if SUBSYSTEMS.contains(&value) => relevant = true,
            "USB_STATE" => relevant = true,
            _ => {}
        }
    }
    Ok(Msg::Kernel {
        seqnum: seqnum.ok_or(())?,
        relevant,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Nothing,
    /// A USB-related uevent on a live stream: mint a new epoch.
    Bump,
    /// Revoke; a new socket and snapshot are needed.
    Lost,
}

/// Pure event -> epoch logic for one socket.
#[derive(Default)]
pub(crate) struct Tracker {
    /// The UDC snapshot was taken after binding.
    pub(crate) live: bool,
    last: Option<u64>,
}

impl Tracker {
    /// The snapshot was read after the socket was bound: from here nothing can be missed
    /// without the sequence showing it.
    pub(crate) fn snapshot(&mut self) {
        self.live = true;
    }

    pub(crate) fn on(&mut self, msg: Result<Msg, ()>) -> Step {
        let (seqnum, relevant) = match msg {
            Err(()) => return Step::Lost,
            Ok(Msg::Libudev) => return Step::Nothing,
            Ok(Msg::Kernel { seqnum, relevant }) => (seqnum, relevant),
        };
        if self
            .last
            .is_some_and(|last| last.checked_add(1) != Some(seqnum))
        {
            return Step::Lost;
        }
        self.last = Some(seqnum);
        // Before the snapshot nothing is minted; the mint after the snapshot covers it.
        if relevant && self.live {
            Step::Bump
        } else {
            Step::Nothing
        }
    }
}

/// `/sys/class/udc/*/state`, sorted. `None` when there is no UDC to read: fail closed.
#[cfg(target_os = "linux")]
fn snapshot() -> Option<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = std::fs::read_dir("/sys/class/udc")
        .ok()?
        .filter_map(|e| {
            let e = e.ok()?;
            let state = std::fs::read_to_string(e.path().join("state")).ok()?;
            Some((
                e.file_name().to_string_lossy().into_owned(),
                state.trim().into(),
            ))
        })
        .collect();
    out.sort();
    (!out.is_empty()).then_some(out)
}

#[cfg(target_os = "linux")]
pub(crate) fn spawn(hub: std::sync::Arc<crate::block::Hub>) {
    let _ = std::thread::Builder::new()
        .name("luyaz-uevent".into())
        .spawn(move || {
            let mut backoff = 5u64;
            loop {
                let reached_live = run(&hub);
                hub.firmware().usb_live(false);
                if reached_live {
                    backoff = 5;
                }
                eprintln!("luyaz-core: uevent USB 代次已撤销，{backoff}s 后重新订阅");
                std::thread::sleep(std::time::Duration::from_secs(backoff));
                backoff = (backoff * 2).min(300);
            }
        });
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn spawn(_hub: std::sync::Arc<crate::block::Hub>) {}

/// One socket until it is lost. Returns whether it ever went live.
#[cfg(target_os = "linux")]
fn run(hub: &crate::block::Hub) -> bool {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let mut tracker = Tracker::default();
    // SAFETY: plain socket syscalls on a descriptor we own; buffers outlive each call.
    unsafe {
        let fd = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            libc::NETLINK_KOBJECT_UEVENT,
        );
        if fd < 0 {
            return false;
        }
        let fd = OwnedFd::from_raw_fd(fd);
        let mut sa: libc::sockaddr_nl = std::mem::zeroed();
        sa.nl_family = libc::AF_NETLINK as u16;
        sa.nl_groups = 1;
        let sl = std::mem::size_of::<libc::sockaddr_nl>() as u32;
        if libc::bind(fd.as_raw_fd(), &sa as *const _ as *const libc::sockaddr, sl) < 0 {
            return false;
        }
        // Bound; now the snapshot. Uevents queued meanwhile are read next and bump.
        let Some(udc) = snapshot() else {
            return false;
        };
        eprintln!("luyaz-core: uevent 已订阅，UDC {udc:?}");
        tracker.snapshot();
        hub.firmware().usb_live(true);
        let mut buf = vec![0u8; MAX_MSG + 1];
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
            // ENOBUFS (lost multicast), any error, EOF: revoke. Oversize is parse's to reject.
            if n <= 0 {
                return tracker.live;
            }
            let n = (n as usize).min(buf.len());
            match tracker.on(parse(&buf[..n])) {
                Step::Nothing => {}
                Step::Bump => hub.firmware().usb_event(),
                Step::Lost => return tracker.live,
            }
        }
    }
}

#[cfg(test)]
#[path = "uevent_tests.rs"]
mod tests;
