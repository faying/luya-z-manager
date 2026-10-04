use std::process::Command;
use std::sync::Mutex;

const PORTS: &[&str] = &[
    "/dev/at_mdm0",
    "/dev/at_mdm1",
    "/dev/at_usb0",
    "/dev/smd7",
    "/dev/smd11",
];

pub struct AtPort {
    cached: Mutex<Option<String>>,
}

impl AtPort {
    pub fn new() -> Self {
        Self {
            cached: Mutex::new(None),
        }
    }

    /// Probe (or return the cached) AT port. Takes the port lock: the probe
    /// itself writes "AT" and reads the port, like any other command.
    pub fn detect(&self) -> Option<String> {
        // Known port: answer without queueing behind a running command.
        if let Some(p) = self.cached.lock().unwrap().clone() {
            return Some(p);
        }
        let _serial = PORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _flock = port_flock();
        self.detect_locked()
    }

    fn detect_locked(&self) -> Option<String> {
        {
            let cached = self.cached.lock().unwrap();
            if let Some(ref port) = *cached {
                return Some(port.clone());
            }
        }

        for &port in PORTS {
            if !std::path::Path::new(port).exists() {
                continue;
            }
            let script = format!(
                "cat {p} & PID=$! ; sleep 0.3 ; echo -e 'AT\\r' > {p} ; sleep 1 ; kill $PID 2>/dev/null",
                p = port
            );
            let output = Command::new("sh")
                .args(["-c", &script])
                .output()
                .ok();
            if let Some(out) = output {
                let resp = String::from_utf8_lossy(&out.stdout);
                if resp.contains("OK") {
                    let mut cached = self.cached.lock().unwrap();
                    *cached = Some(port.to_string());
                    return Some(port.to_string());
                }
            }
        }
        None
    }
}

/// One command on the port at a time, process-wide: two `cat` readers on the
/// same tty take each other's replies (several AtPort instances exist).
static PORT_LOCK: Mutex<()> = Mutex::new(());

/// …and across processes: datad sends AT+COPS=0 / AT+CFUN=1 on the same port
/// (write-op-layer.md T7b) and holds the same flock. `ZTE_AGENT_AT_LOCK`,
/// default `/var/run/u60-at.lock`; empty = none. Waits up to 15 s (datad's
/// longest command is 6 s), then goes ahead and says so.
struct PortFlock(Option<std::fs::File>);

impl Drop for PortFlock {
    fn drop(&mut self) {
        if let Some(f) = &self.0 {
            use std::os::fd::AsRawFd;
            // SAFETY: fd is the open lock file
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

fn port_flock() -> PortFlock {
    use std::os::fd::AsRawFd;
    let path = std::env::var("ZTE_AGENT_AT_LOCK").unwrap_or_else(|_| "/var/run/u60-at.lock".into());
    if path.is_empty() {
        return PortFlock(None);
    }
    let Ok(f) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path) else {
        return PortFlock(None);
    };
    let start = std::time::Instant::now();
    loop {
        // SAFETY: fd is the open lock file
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return PortFlock(Some(f));
        }
        if start.elapsed() >= std::time::Duration::from_secs(15) {
            eprintln!("[at_cmd] {path} still held after 15 s, using the port anyway");
            return PortFlock(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Send an AT command and return the raw response text.
pub fn send(at_port: &AtPort, command: &str, timeout_secs: u64) -> Result<String, String> {
    let _serial = PORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _flock = port_flock();
    let port = at_port.detect_locked().ok_or("no serial port found")?;
    let script = format!(
        "cat {p} & PID=$! ; sleep 0.3 ; echo -e '{cmd}\\r' > {p} ; sleep {t} ; kill $PID 2>/dev/null",
        p = port,
        cmd = command,
        t = timeout_secs
    );
    let output = Command::new("sh")
        .args(["-c", &script])
        .output()
        .map_err(|e| format!("failed to open port: {e}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
