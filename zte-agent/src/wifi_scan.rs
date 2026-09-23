// ─────────────────────────────────────────────────────────────────────────────
// wifi_scan — list nearby networks without disturbing anything.
//
// Two situations have to work:
//
//  * APs are up (the "away" scenario). Scan straight on an existing AP
//    interface. `iw dev wlanN scan` does an off-channel sweep and returns; the
//    AP keeps running. This is what homemode has always done.
//  * APs are down (the "home" scenario). There is no AP interface to scan on,
//    so add a temporary `managed` vdev to the phy, scan, and remove it.
//
// Two things measured on-device 2026-09-22 shape this, both of which contradict
// what the documentation suggests:
//
//  * `iw list`'s "valid interface combinations" is advisory on this chip. It
//    claims a managed vdev may only coexist with a single AP; in practice
//    adding one alongside *two* running APs succeeded and scanned normally for
//    well over a minute. Do not reason from that table.
//  * The phy index is not stable. Every `zwrt_wlan reload` tears the wiphy down
//    and re-registers it with a fresh index (phy2 → … → phy9 over one session),
//    and a temporary vdev does not survive that. So: discover the phy on each
//    use, never cache it, and never expect a scan vdev to outlive a Wi-Fi
//    reconfiguration.
//
// Nothing here writes uci. That is deliberate — it keeps the detector free of
// side effects, so the engine can never mistake its own scan for the user
// having changed something by hand.
// ─────────────────────────────────────────────────────────────────────────────

use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

/// Name for the temporary scan vdev. Distinct from homemode's older probe names
/// so a leftover from one cannot be mistaken for the other.
const SCAN_IFACE: &str = "scen-scan0";
const SCAN_ATTEMPTS: u32 = 3;
const SCAN_RETRY_DELAY: Duration = Duration::from_millis(1500);

#[derive(Clone, Debug)]
pub struct Network {
    pub ssid: String,
    pub bssid: String,
    pub signal: f64,
}

impl Network {
    pub fn to_json(&self) -> Value {
        json!({"ssid": self.ssid, "bssid": self.bssid, "signal": self.signal})
    }
}

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{cmd} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Interfaces `iw dev` currently knows about, in the order it lists them.
fn existing_ifaces() -> Vec<String> {
    run("iw", &["dev"])
        .map(|text| {
            text.lines()
                .filter_map(|l| l.trim().strip_prefix("Interface ").map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// First phy `iw phy` reports. Re-read every time: the index changes on every
/// Wi-Fi reload.
fn first_phy() -> Option<String> {
    run("iw", &["phy"]).ok().and_then(|text| {
        text.lines()
            .find_map(|l| l.strip_prefix("Wiphy ").map(|s| s.trim().to_string()))
    })
}

/// Parse `iw dev … scan` output.
///
/// The shape is a `BSS <mac>(on <iface>)` header followed by indented fields,
/// of which we want `signal:` and `SSID:`. homemode's parser reads only the
/// latter two and throws the BSS line away, which is why it cannot support
/// BSSID matching; this one keeps it.
///
/// Real output contains hidden networks (empty SSID) and SSIDs with escaped
/// bytes (`\x00…`, UTF-8 shown as `\xef\xbc\x88…`). Hidden ones are dropped —
/// they can never match a configured name. Escapes are left exactly as `iw`
/// printed them so a configured SSID copied from this output still compares
/// equal.
pub fn parse_scan(text: &str) -> Vec<Network> {
    let mut out: Vec<Network> = Vec::new();
    let mut bssid = String::new();
    let mut signal: Option<f64> = None;

    let flush = |out: &mut Vec<Network>, bssid: &str, signal: Option<f64>, ssid: String| {
        if ssid.is_empty() || bssid.is_empty() {
            return;
        }
        let sig = signal.unwrap_or(-100.0);
        // One entry per BSSID; if the same BSSID somehow repeats, keep the
        // stronger reading.
        match out.iter_mut().find(|n| n.bssid == bssid) {
            Some(existing) => {
                if sig > existing.signal {
                    existing.signal = sig;
                    existing.ssid = ssid;
                }
            }
            None => out.push(Network {
                ssid,
                bssid: bssid.to_string(),
                signal: sig,
            }),
        }
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("BSS ") {
            // "02:00:00:00:00:aa(on wlan0)" — also tolerate a bare mac.
            bssid = rest
                .split('(')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            signal = None;
        } else if let Some(rest) = trimmed.strip_prefix("signal:") {
            signal = rest
                .trim()
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok());
        } else if let Some(rest) = trimmed.strip_prefix("SSID:") {
            flush(&mut out, &bssid, signal, rest.trim().to_string());
        }
    }

    out.sort_by(|a, b| b.signal.partial_cmp(&a.signal).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// A freshly created (or freshly woken) interface often returns an empty list
/// on the first try, so retry a couple of times before believing it.
fn scan_on(iface: &str) -> Result<Vec<Network>, String> {
    let mut last_err = String::new();
    for attempt in 0..SCAN_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(SCAN_RETRY_DELAY);
        }
        match run("iw", &["dev", iface, "scan"]) {
            Ok(text) => {
                let nets = parse_scan(&text);
                if !nets.is_empty() {
                    return Ok(nets);
                }
                last_err = format!("scan on {iface} returned no networks");
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// Remove the temporary vdev. Best effort — if it is already gone (a Wi-Fi
/// reload took the whole wiphy with it) that is not an error.
fn drop_scan_iface() {
    let _ = Command::new("iw").args(["dev", SCAN_IFACE, "del"]).output();
}

/// Scan for nearby networks.
///
/// Prefers an interface that already exists; only creates (and always removes)
/// a temporary managed vdev when there is none. Writes no configuration either
/// way.
pub fn scan() -> Result<Vec<Network>, String> {
    // A leftover from a previous run that died mid-scan would otherwise make
    // `interface add` fail with EEXIST.
    let ifaces = existing_ifaces();
    if ifaces.iter().any(|i| i == SCAN_IFACE) {
        drop_scan_iface();
    }

    if let Some(iface) = existing_ifaces().into_iter().find(|i| i != SCAN_IFACE) {
        if let Ok(nets) = scan_on(&iface) {
            return Ok(nets);
        }
        // Fall through: the interface exists but could not scan (it may be
        // mid-teardown). Try the dedicated vdev instead.
    }

    let phy = first_phy().ok_or(
        "no wiphy present — the radios are fully torn down, nothing can scan until they return",
    )?;
    run("iw", &["phy", &phy, "interface", "add", SCAN_IFACE, "type", "managed"])?;
    let result = (|| {
        run("ip", &["link", "set", SCAN_IFACE, "up"])?;
        std::thread::sleep(Duration::from_millis(800));
        scan_on(SCAN_IFACE)
    })();
    drop_scan_iface();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shaped like real `iw dev scan` output, including the two shapes that
    // broke naive parsers: a hidden network with an empty SSID, and an SSID
    // printed with escape sequences. Addresses and names are made up
    // (02:00:00:00:00:xx is locally administered, never a real radio): real
    // BSSIDs can be looked up in wardriving databases and resolve to a street.
    const SAMPLE: &str = r#"
BSS 02:00:00:00:00:01(on scen-scan0)
	TSF: 1 usec
	signal: -29.00 dBm
	SSID: Chill?
BSS 02:00:00:00:00:02(on scen-scan0)
	signal: -85.00 dBm
	SSID:
BSS 02:00:00:00:00:03(on scen-scan0)
	signal: -78.00 dBm
	SSID: \x00\x00\x00\x00
BSS 02:00:00:00:00:04(on scen-scan0)
	signal: -62.00 dBm
	SSID: Neighbour-5G
"#;

    #[test]
    fn parses_bssid_signal_and_ssid() {
        let nets = parse_scan(SAMPLE);
        // The hidden network (empty SSID) is dropped; the other three remain.
        assert_eq!(nets.len(), 3);
        // Sorted strongest first.
        assert_eq!(nets[0].ssid, "Chill?");
        assert_eq!(nets[0].bssid, "02:00:00:00:00:01");
        assert_eq!(nets[0].signal, -29.0);
        assert_eq!(nets[1].ssid, "Neighbour-5G");
        // Escaped bytes are preserved verbatim rather than mangled.
        assert!(nets.iter().any(|n| n.ssid == r"\x00\x00\x00\x00"));
    }

    #[test]
    fn ignores_entries_without_a_bss_header() {
        // A stray SSID line with no preceding BSS must not invent a network.
        let nets = parse_scan("\tSSID: orphan\n");
        assert!(nets.is_empty());
    }

    #[test]
    fn missing_signal_defaults_to_very_weak() {
        let nets = parse_scan("BSS 02:00:00:00:00:aa(on x)\n\tSSID: nosignal\n");
        assert_eq!(nets.len(), 1);
        assert_eq!(nets[0].signal, -100.0);
    }
}
