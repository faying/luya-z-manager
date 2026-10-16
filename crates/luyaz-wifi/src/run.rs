//! A command with a deadline: zte-agent `ubus::output_within` (without the process-group variant),
//! so a `hostapd_cli` that hangs cannot hold the Wi-Fi lock.

use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Run `cmd` (stdin closed, output captured), killing it after `limit`.
pub fn output_within(cmd: &mut Command, limit: Duration) -> io::Result<Output> {
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

fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
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
}
