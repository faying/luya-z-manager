use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;

/// `ubus -t` for [`read`]. The CLI's own default is 30 s, and the HTTP server
/// has only two worker threads (audit 2026-10-04 P1-6). Observed vendor
/// latencies are 0–15 ms, so 10 s only ever cuts off a call that is stuck.
pub const READ_TIMEOUT: u32 = 10;

/// For the reads with no measured upper bound that may be big: an SMS list
/// page is up to 500 rows. Same as the CLI default, so no change for them.
pub const LONG_READ_TIMEOUT: u32 = 30;

/// Bound for the other quick local commands here (`ubus -v list`, `uci get`).
pub const CMD_TIMEOUT: Duration = Duration::from_secs(5);

/// Our own deadline is `-t` plus this: `-t` covers the call, not connecting
/// to a ubusd that does not answer.
const BACKSTOP: Duration = Duration::from_secs(2);

/// Read-only `ubus call <object> <method> [<params>]`, JSON output parsed.
/// Refuses any method that does not look like a read ([`is_read_method`]):
/// every device write goes through datad (`datad_write`; write-op-layer.md
/// T7). There is no direct ubus write here any more; `tests/no_direct_writes.rs`
/// keeps it that way.
/// Gives up after [`READ_TIMEOUT`].
pub fn read(object: &str, method: &str, params: Option<&str>) -> Result<Value, String> {
    read_with_timeout(object, method, params, None)
}

/// [`read`], giving up after `secs` (`ubus -t`); `None` = [`READ_TIMEOUT`].
pub fn read_with_timeout(object: &str, method: &str, params: Option<&str>, secs: Option<u32>) -> Result<Value, String> {
    if !is_read_method(method) {
        return Err(format!("ubus {object} {method}: not a read; writes go through datad"));
    }
    run(object, method, params, secs)
}

/// Methods that only read, by the vendor's naming: `get`/`list`/`status`/
/// `info`/`report` and the netselect polls (`…_status`, `…_result`, `…_contents`).
pub fn is_read_method(method: &str) -> bool {
    let m = method.to_ascii_lowercase();
    matches!(m.as_str(), "list" | "status" | "info" | "report")
        || m.starts_with("get")
        || m.contains("_get_")
        || m.contains("get_")
        || ["_status", "_result", "_contents"].iter().any(|s| m.ends_with(s))
}

fn run(object: &str, method: &str, params: Option<&str>, secs: Option<u32>) -> Result<Value, String> {
    let secs = secs.unwrap_or(READ_TIMEOUT);
    let mut cmd = Command::new("ubus");
    cmd.args(["-t", &secs.to_string()]);
    cmd.args(["call", object, method]);
    if let Some(p) = params {
        cmd.arg(p);
    }
    let output = output_within(&mut cmd, Duration::from_secs(secs.into()) + BACKSTOP)
        .map_err(|e| format!("ubus call {object} {method}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = match stderr.find("\nUsage:") {
            Some(pos) => &stderr[..pos],
            None => &stderr,
        };
        return Err(format!(
            "ubus call {object} {method} failed: {}",
            stderr.trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(trimmed).map_err(|e| format!("ubus JSON parse: {e}"))
}

/// Does `object` offer `method`? From `ubus -v list <object>`, which only
/// describes, it calls nothing. For setters a firmware dropped (B31 removed
/// `zwrt_bsp.usb set`), so a page can say so before anyone presses the button.
/// None when the list could not be read.
pub fn has_method(object: &str, method: &str) -> Option<bool> {
    let output = output_within(Command::new("ubus").args(["-v", "list", object]), CMD_TIMEOUT).ok()?;
    if !output.status.success() {
        return None;
    }
    Some(lists_method(&String::from_utf8_lossy(&output.stdout), method))
}

/// `ubus -v list` prints one `"<method>":{<args>}` line per method.
fn lists_method(listing: &str, method: &str) -> bool {
    let want = format!("\"{method}\":");
    listing.lines().any(|l| l.trim_start().starts_with(&want))
}

/// Run `uci get <key>` and return the value.
pub fn uci_get(key: &str) -> Result<String, String> {
    let output = output_within(Command::new("uci").args(["get", key]), CMD_TIMEOUT)
        .map_err(|e| format!("uci get {key}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("uci get {key}: {stderr}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// [`Command::output`] with a deadline: past `limit` the child is killed and
/// this returns `ErrorKind::TimedOut`. stdin is null; stdout and stderr are
/// drained on their own threads, so a chatty child cannot fill a pipe and
/// stall. Only for direct execs: through `sh -c` the kill reaches the shell,
/// not what it started, which keeps running (and holding the pipes).
pub fn output_within(cmd: &mut Command, limit: Duration) -> io::Result<Output> {
    output_bounded(cmd, limit, false)
}

/// [`output_within`] for a command that starts children of its own (`sh -c`
/// with a pipe, a script): the child leads a new process group and a timeout
/// kills the whole group. Killing only the shell would leave the pipeline
/// running, still holding the pipes (audit 2026-10-04 P2-2, P2-5).
pub fn output_within_group(cmd: &mut Command, limit: Duration) -> io::Result<Output> {
    output_bounded(cmd, limit, true)
}

fn output_bounded(cmd: &mut Command, limit: Duration, group: bool) -> io::Result<Output> {
    if group {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
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
                // Before wait(): once reaped, the id could be someone else's.
                // Never kill(-0): that is our own group.
                let pid = child.id() as libc::pid_t;
                if group && pid > 0 {
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
    fn reads_and_writes_by_name() {
        for m in ["list", "status", "info", "report", "get_wwaniface", "nwinfo_get_netinfo", "router_get_qos", "getHostHints",
            "zte_libwms_get_sms_data", "wlan_get_guest_access_left_time", "nwinfo_m_netselect_status", "nwinfo_m_netselect_result",
            "nwinfo_m_netselect_contents", "get_apn_at_cid"] {
            assert!(is_read_method(m), "{m}");
        }
        for m in ["set", "reload", "set_wwaniface", "nwinfo_set_netselect", "nwinfo_rest_band_rat", "nwinfo_reset_band_cell_setting",
            "enable_manu_apn_id", "add_manu_apn", "zte_libwms_send_sms", "zwrt_wms_delete_sms", "zwrt_wms_modify_tag", "device_reboot",
            "nwinfo_manual_scan", "nwinfo_manual_register", "set_qcliiface", "enableAutoSleep", "router_set_lan_para", "reboot"] {
            assert!(!is_read_method(m), "{m}");
        }
        assert!(read("zwrt_data", "set_wwaniface", None).unwrap_err().contains("not a read"));
    }

    #[test]
    fn finds_methods_in_a_listing() {
        let b31 = "'zwrt_bsp.usb' @8577ea77\n\t\"list\":{}\n";
        let b27 = "'zwrt_bsp.usb' @21414706\n        \"list\":{}\n        \"set\":{\"mode\":\"String\"}\n";
        assert!(lists_method(b31, "list"));
        assert!(!lists_method(b31, "set"));
        assert!(lists_method(b27, "set"));
        assert!(!lists_method(b27, "se"));
    }

    #[test]
    fn output_within_returns_both_streams_and_status() {
        let out = output_within(Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"]), Duration::from_secs(5)).unwrap();
        assert_eq!(out.stdout, b"out\n");
        assert_eq!(out.stderr, b"err\n");
        assert_eq!(out.status.code(), Some(3));
    }

    #[test]
    fn output_within_kills_a_child_past_the_deadline() {
        let t0 = Instant::now();
        let e = output_within(Command::new("sleep").arg("10"), Duration::from_millis(200)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
    }

    #[test]
    fn output_within_does_not_stall_on_a_full_pipe() {
        // ~200 KB on each stream, well past a 64 KB pipe buffer.
        let script = "head -c 200000 /dev/zero; head -c 200000 /dev/zero >&2";
        let out = output_within(Command::new("sh").args(["-c", script]), Duration::from_secs(10)).unwrap();
        assert_eq!((out.stdout.len(), out.stderr.len()), (200_000, 200_000));
        assert!(out.status.success());
    }
}
