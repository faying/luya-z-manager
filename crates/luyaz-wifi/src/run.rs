//! A command with a deadline, so a `hostapd_cli` that hangs cannot hold the Wi-Fi lock. The command
//! runs in its own process group and a timeout kills the whole group: a `sh -c` caller's grandchildren
//! go with it instead of piling up on every periodic call.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
/// Each stream keeps at most this much; the rest is read and dropped so the command never blocks on
/// a full pipe.
pub const MAX_OUTPUT: usize = 1 << 20;

/// Run `cmd` (stdin closed, output captured, own process group), killing the group after `limit`.
pub fn output_within(cmd: &mut Command, limit: Duration) -> io::Result<Output> {
    cmd.process_group(0);
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let t0 = Instant::now();
    let mut nap = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if t0.elapsed() < limit => {
                thread::sleep(nap);
                nap = (nap * 2).min(Duration::from_millis(20));
            }
            other => {
                // Before wait(): once reaped, the id could be someone else's. Never kill(-0): that
                // is our own group.
                let pid = child.id() as libc::pid_t;
                if pid > 0 {
                    unsafe { libc::kill(-pid, libc::SIGKILL) };
                }
                let _ = child.kill();
                let _ = child.wait();
                // The readers end when the pipes close; not joined, in case
                // something else still holds them.
                return Err(match other {
                    Err(e) => e,
                    _ => io::Error::new(io::ErrorKind::TimedOut, format!("timed out after {} ms", limit.as_millis())),
                });
            }
        }
    };
    let join = |h: Option<JoinHandle<Vec<u8>>>| h.and_then(|h| h.join().ok()).unwrap_or_default();
    Ok(Output { status, stdout: join(stdout), stderr: join(stderr) })
}

/// SIGKILL `child`'s whole process group (it must lead one: `process_group(0)` or `setsid`), then
/// reap it.
pub fn kill_group(child: &mut std::process::Child) {
    let pid = child.id() as libc::pid_t;
    if pid > 0 {
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// A one-at-a-time slot for a background child and when it started (core's and the sentinel's
/// recover-live).
pub type Slot = Option<(std::process::Child, Instant)>;

/// What [`check_slot`] found.
#[derive(Debug, PartialEq)]
pub enum SlotState {
    /// Nothing was there.
    Free,
    /// Still running within `cap` (pid): leave it.
    Running(u32),
    /// Ended by itself (its status): the slot is free.
    Ended(Option<std::process::ExitStatus>),
    /// Still running past `cap` (pid): its group was killed, the slot is free.
    Killed(u32),
}

/// Look at `slot`: free it when its child ended or ran past `cap` (then kill its group).
pub fn check_slot(slot: &mut Slot, cap: Duration) -> SlotState {
    let Some((ch, t0)) = slot.as_mut() else { return SlotState::Free };
    let st = match ch.try_wait() {
        Ok(None) if t0.elapsed() < cap => return SlotState::Running(ch.id()),
        Ok(None) => {
            let pid = ch.id();
            kill_group(ch);
            SlotState::Killed(pid)
        }
        Ok(Some(st)) => SlotState::Ended(Some(st)),
        Err(_) => SlotState::Ended(None),
    };
    *slot = None;
    st
}

fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut pipe).take(MAX_OUTPUT as u64).read_to_end(&mut buf);
        let _ = io::copy(&mut pipe, &mut io::sink());
        buf
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_output_and_times_out() {
        let o = output_within(Command::new("sh").args(["-c", "echo hi"]), Duration::from_secs(5)).unwrap();
        assert_eq!(String::from_utf8_lossy(&o.stdout), "hi\n");
        let t0 = Instant::now();
        let e = output_within(Command::new("sleep").arg("5"), Duration::from_millis(200)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(t0.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_timeout_takes_the_grandchildren_too() {
        let f = std::env::temp_dir().join(format!("wifi-run-{}", std::process::id()));
        let script = format!("sleep 600 & echo $! > {0}; sleep 600 & echo $! >> {0}; wait", f.display());
        let e = output_within(Command::new("sh").args(["-c", &script]), Duration::from_millis(300)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        let pids = std::fs::read_to_string(&f).unwrap();
        let _ = std::fs::remove_file(&f);
        assert_eq!(pids.lines().count(), 2);
        thread::sleep(Duration::from_millis(100));
        for pid in pids.lines() {
            // gone, or a zombie waiting for an init that doesn't reap
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            assert!(stat.is_empty() || stat.contains(") Z "), "sleep {pid} still running: {stat}");
        }
    }

    #[test]
    fn output_is_capped_and_the_command_still_finishes() {
        let o = output_within(
            Command::new("sh").args(["-c", "head -c 5000000 /dev/zero; echo done >&2"]),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(o.status.success());
        assert_eq!(o.stdout.len(), MAX_OUTPUT);
        assert_eq!(String::from_utf8_lossy(&o.stderr), "done\n");
    }

    #[test]
    fn a_slot_past_its_cap_is_killed_with_its_group() {
        let spawn = |script: &str| {
            let mut c = Command::new("sh");
            c.args(["-c", script]).process_group(0);
            Some((c.spawn().unwrap(), Instant::now()))
        };
        let mut slot: Slot = None;
        assert_eq!(check_slot(&mut slot, Duration::from_secs(1)), SlotState::Free);
        slot = spawn("exit 3");
        thread::sleep(Duration::from_millis(200));
        assert!(matches!(check_slot(&mut slot, Duration::from_secs(60)), SlotState::Ended(Some(s)) if s.code() == Some(3)));
        assert!(slot.is_none());

        let f = std::env::temp_dir().join(format!("wifi-slot-{}", std::process::id()));
        slot = spawn(&format!("sleep 600 & echo $! > {}; wait", f.display()));
        thread::sleep(Duration::from_millis(200));
        assert!(matches!(check_slot(&mut slot, Duration::from_secs(60)), SlotState::Running(_)), "within the cap: left alone");
        assert!(slot.is_some());
        assert!(matches!(check_slot(&mut slot, Duration::from_millis(100)), SlotState::Killed(_)));
        assert!(slot.is_none(), "freed for the next one");
        let pid = std::fs::read_to_string(&f).unwrap();
        let _ = std::fs::remove_file(&f);
        thread::sleep(Duration::from_millis(100));
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())).unwrap_or_default();
        assert!(stat.is_empty() || stat.contains(") Z "), "its child still runs: {stat}");
    }
}
