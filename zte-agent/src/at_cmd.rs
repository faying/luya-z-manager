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

/// Send an AT command and return the raw response text.
pub fn send(at_port: &AtPort, command: &str, timeout_secs: u64) -> Result<String, String> {
    let _serial = PORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
