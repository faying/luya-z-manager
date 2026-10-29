//! The resident AT port (luyaz-phone design R11, R15, R19). A port stays open once a command
//! has used it, and one reader thread per port sleeps in `poll(2)` until the modem says
//! something: no timer, so no wake-ups while nothing happens.
//!
//! Each whole line goes to whoever owns it:
//! - with a command in flight, its lines and its final code are its answer;
//! - a known unsolicited line ([`URC`]: RING, +CLIP, +CMTI…) is broadcast to [`subscribe`]rs,
//!   even mid-answer, unless the command in flight asked for that prefix itself (`AT+CLIP?`);
//! - anything else with no command in flight is a late reply nobody waits for: dropped.
//!
//! This replaces "drain whatever is unread, then write" (which threw a RING away whenever it
//! landed between two commands). A command that ends without its final code leaves the port
//! dirty: before the next one, a marked `AT+CMEE?` is sent and everything up to its own answer
//! is dropped, so a late OK is never taken as the next command's.
//!
//! One reader per port, in this process. The cross-process flock still serialises commands;
//! the only other user of the port, `luyaz-fallback.sh`, opens it only when core is gone (FC7),
//! so the resident reader never takes its replies.

#![allow(dead_code)]

use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::{fs::OpenOptionsExt, io::AsRawFd},
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant},
};

use super::{ANSWER_MAX, BUSY, LOCK_WAIT, READ_BUF, Until, codes, complete};

/// Unsolicited lines: incoming calls, call ends, new SMS, IMS registration. `+CLCC` is not one:
/// it is only ever the answer to `AT+CLCC` (R19); `+CUSD` and STK lines stay with the commands
/// that wait for them.
pub const URC: &[&str] = &[
    "RING",
    "+CRING:",
    "+CLIP:",
    "+CCWA:",
    "+CMTI:",
    "+CDSI:",
    "+CIREPI:",
    "+CIREPH:",
    "+CIREGU:",
    "NO CARRIER",
    "BUSY",
    "NO ANSWER",
];
/// The result codes a call command (`ATA`, `ATD…`, `ATH`, `AT+CHUP`) may end with: its own.
const CALL_RESULTS: &[&str] = &["NO CARRIER", "BUSY", "NO ANSWER"];
/// The marked command of a resync: a fixed answer line no other command gives.
const RESYNC: &str = "AT+CMEE?";
const RESYNC_MARK: &str = "+CMEE:";
const RESYNC_WAIT: Duration = Duration::from_secs(2);

/// An unsolicited line, as it came, and the port it came on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Urc {
    pub port: String,
    pub line: String,
}

fn urc_tx() -> &'static tokio::sync::broadcast::Sender<Urc> {
    static TX: OnceLock<tokio::sync::broadcast::Sender<Urc>> = OnceLock::new();
    TX.get_or_init(|| tokio::sync::broadcast::channel(64).0)
}

/// Every unsolicited line from now on (a slow reader loses the oldest, `Lagged`).
pub fn subscribe() -> tokio::sync::broadcast::Receiver<Urc> {
    urc_tx().subscribe()
}

pub(super) struct Port {
    path: String,
    file: File,
    state: Mutex<State>,
    cv: Condvar,
}

#[derive(Default)]
struct State {
    /// what came after the last line break
    partial: Vec<u8>,
    flight: Option<Flight>,
    /// the reader stopped (read error, hang-up): the next command opens the port again
    dead: Option<String>,
    /// a command is on the port (the flock serialises processes; this, threads)
    busy: bool,
    /// the last command ended without its final code: resync first
    dirty: bool,
}

struct Flight {
    own: Vec<String>,
    answer: String,
}

fn ports() -> MutexGuard<'static, HashMap<String, Arc<Port>>> {
    static PORTS: OnceLock<Mutex<HashMap<String, Arc<Port>>>> = OnceLock::new();
    PORTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The open port at `path`, if one is open and its reader is alive (the probe uses it so the
/// reader does not take the probe's answer).
pub(super) fn resident(path: &str) -> Option<Arc<Port>> {
    ports().get(path).filter(|p| p.alive()).cloned()
}

/// The port at `path`, opened (again, if its reader stopped) when needed.
pub(super) fn get(path: &str) -> Result<Arc<Port>, String> {
    let mut map = ports();
    if let Some(p) = map.get(path).filter(|p| p.alive()) {
        return Ok(p.clone());
    }
    let port = open(path)?;
    map.insert(path.to_string(), port.clone());
    Ok(port)
}

fn open(path: &str) -> Result<Arc<Port>, String> {
    // O_NONBLOCK only for the open (a tty may wait for carrier); then blocking: on B31 a
    // non-blocking write to at_mdm2 always says EBUSY (10-13), and the reader sleeps in poll
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
        .map_err(|e| format!("open {path}: {e}"))?;
    super::set_nonblocking(&f, false);
    let reader = f.try_clone().map_err(|e| format!("open {path}: {e}"))?;
    let port = Arc::new(Port {
        path: path.to_string(),
        file: f,
        state: Mutex::new(State::default()),
        cv: Condvar::new(),
    });
    let p = port.clone();
    std::thread::Builder::new()
        .name("at-read".into())
        .spawn(move || p.read_loop(reader))
        .map_err(|e| format!("AT reader {path}: {e}"))?;
    Ok(port)
}

/// The prefixes `text`'s own answer lines start with: `AT+CLCC` → `+CLCC`; a call command's
/// result codes too.
fn own_prefixes(text: &str) -> Vec<String> {
    let t = text.trim().to_ascii_uppercase();
    let mut own = Vec::new();
    let Some(rest) = t.strip_prefix("AT") else {
        return own;
    };
    if rest.starts_with(['+', '^', '$']) {
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || "+^$".contains(*c))
            .collect();
        if name.len() > 1 {
            own.push(name);
        }
    }
    if rest == "A" || rest.starts_with('D') || rest.starts_with('H') || rest.starts_with("+CHUP") {
        own.extend(CALL_RESULTS.iter().map(|s| s.to_string()));
    }
    own
}

/// A whole line of `s` is one of a call command's result codes.
fn call_ended(s: &str) -> bool {
    s.lines().any(|l| CALL_RESULTS.contains(&l.trim()))
}

impl Port {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn alive(&self) -> bool {
        self.lock().dead.is_none()
    }

    fn read_loop(self: Arc<Self>, mut f: File) {
        let mut buf = vec![0u8; READ_BUF];
        let why = loop {
            let mut p = libc::pollfd {
                fd: f.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd for a descriptor this thread owns.
            let r = unsafe { libc::poll(&mut p, 1, -1) };
            if r < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break format!("poll: {e}");
            }
            if p.revents & libc::POLLIN == 0
                && p.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
            {
                break "hung up".to_string();
            }
            // one read per message: the smd port hands a reply over whole and drops what does
            // not fit (10-13: a 549-byte +CGCONTRDP read into 512 bytes lost its tail and the OK)
            match f.read(&mut buf) {
                Ok(0) => break "closed".to_string(),
                Ok(n) => self.feed(&buf[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(e) => break format!("read: {e}"),
            }
        };
        eprintln!(
            "[at] {}: {why}: the port is opened again by the next command",
            self.path
        );
        self.lock().dead = Some(why);
        self.cv.notify_all();
    }

    fn feed(&self, bytes: &[u8]) {
        let mut st = self.lock();
        st.partial.extend_from_slice(bytes);
        let mut urcs = Vec::new();
        loop {
            let cut = match st.partial.iter().position(|b| *b == b'\r' || *b == b'\n') {
                Some(i) => i,
                // a line that never ends is cut where the answer limit would cut it
                None if st.partial.len() >= ANSWER_MAX => st.partial.len(),
                None => break,
            };
            let line: Vec<u8> = st.partial.drain(..cut).collect();
            if !st.partial.is_empty() {
                st.partial.remove(0);
            }
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let urc = URC.iter().any(|u| line.starts_with(u));
            if let Some(f) = st.flight.as_mut()
                && (!urc || f.own.iter().any(|o| line.starts_with(o.as_str())))
            {
                if f.answer.len() < ANSWER_MAX {
                    f.answer.push_str("\r\n");
                    f.answer.push_str(line);
                    f.answer.push_str("\r\n");
                }
                continue;
            }
            if urc {
                urcs.push(line.to_string());
            }
        }
        drop(st);
        self.cv.notify_all();
        for line in urcs {
            let _ = urc_tx().send(Urc {
                port: self.path.clone(),
                line,
            });
        }
    }

    /// Write `text\r`, wait until `until` or `wait`: what came, and whether `until` ended it.
    pub(super) fn command(
        &self,
        text: &str,
        wait: Duration,
        until: Until,
    ) -> Result<(String, bool), String> {
        let start = Instant::now();
        let mut st = self.lock();
        while st.busy {
            if let Some(e) = &st.dead {
                return Err(format!("read {}: {e}", self.path));
            }
            let left = LOCK_WAIT.saturating_sub(start.elapsed());
            if left.is_zero() {
                return Err(format!("{BUSY} (another command is running)"));
            }
            st = self
                .cv
                .wait_timeout(st, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        st.busy = true;
        drop(st);
        let r = self.exchange(text, wait, until);
        self.lock().busy = false;
        self.cv.notify_all();
        r
    }

    fn exchange(&self, text: &str, wait: Duration, until: Until) -> Result<(String, bool), String> {
        if self.lock().dirty {
            let (a, _) = self.round(RESYNC, vec![RESYNC_MARK.into()], RESYNC_WAIT, &|s| {
                s.find(RESYNC_MARK)
                    .is_some_and(|i| codes(&s[i..]).next().is_some())
            })?;
            if !a.contains(RESYNC_MARK) {
                return Err(format!(
                    "{text}: the AT port is stuck (no answer to a resync)"
                ));
            }
            self.lock().dirty = false;
        }
        let own = own_prefixes(text);
        // a call command also ends at its own NO CARRIER / BUSY / NO ANSWER
        let call = own.iter().any(|o| o == CALL_RESULTS[0]);
        let over = |s: &str| complete(s, until) || (call && call_ended(s));
        let (answer, ended) = self.round(text, own, wait, &over)?;
        if codes(&answer).next().is_none() && !(call && call_ended(&answer)) {
            self.lock().dirty = true;
        }
        Ok((answer, ended))
    }

    /// One write and its answer: until `done` says so or `wait` is over.
    fn round(
        &self,
        text: &str,
        own: Vec<String>,
        wait: Duration,
        done: &dyn Fn(&str) -> bool,
    ) -> Result<(String, bool), String> {
        let mut st = self.lock();
        if let Some(e) = &st.dead {
            return Err(format!("read {}: {e}", self.path));
        }
        st.flight = Some(Flight {
            own,
            answer: String::new(),
        });
        drop(st);
        if let Err(e) = (&self.file).write_all(format!("{text}\r").as_bytes()) {
            self.lock().flight = None;
            return Err(format!("write {}: {e}", self.path));
        }
        let start = Instant::now();
        let mut st = self.lock();
        let ended = loop {
            if let Some(e) = &st.dead {
                let e = format!("read {}: {e}", self.path);
                st.flight = None;
                return Err(e);
            }
            let answer = st.flight.as_ref().map_or("", |f| f.answer.as_str());
            if done(answer) {
                break true;
            }
            let left = wait.saturating_sub(start.elapsed());
            if left.is_zero() || answer.len() >= ANSWER_MAX {
                break false;
            }
            st = self
                .cv
                .wait_timeout(st, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        };
        let answer = st.flight.take().map(|f| f.answer).unwrap_or_default();
        Ok((answer, ended))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_owns_its_prefix_and_a_call_command_its_result_codes() {
        assert_eq!(own_prefixes("AT+CLCC"), ["+CLCC"]);
        assert_eq!(own_prefixes("at+cusd=1,\"*10#\",15"), ["+CUSD"]);
        assert_eq!(own_prefixes("AT+CLIP?"), ["+CLIP"]);
        assert_eq!(own_prefixes("ATA"), CALL_RESULTS);
        assert_eq!(own_prefixes("ATD+15550100;"), CALL_RESULTS);
        assert_eq!(
            own_prefixes("AT+CHUP"),
            ["+CHUP", "NO CARRIER", "BUSY", "NO ANSWER"]
        );
        assert!(own_prefixes("ATI").is_empty());
        assert!(own_prefixes("AT").is_empty());
    }
}
