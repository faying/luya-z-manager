//! wifi_scan — list nearby networks without disturbing anything.
//!
//! Two situations have to work:
//!
//!  * APs are up (the "away" scenario). Scan straight on an existing AP
//!    interface. `iw dev wlanN scan` does an off-channel sweep and returns; the
//!    AP keeps running.
//!  * APs are down (the "home" scenario). There is no AP interface to scan on,
//!    so add a temporary `managed` vdev to the phy, scan, and remove it.
//!
//! Two things measured on-device 2026-09-22 shape this, both of which contradict
//! what the documentation suggests:
//!
//!  * `iw list`'s "valid interface combinations" is advisory on this chip. It
//!    claims a managed vdev may only coexist with a single AP; in practice
//!    adding one alongside *two* running APs succeeded and scanned normally for
//!    well over a minute. Do not reason from that table.
//!  * The phy index is not stable. Every `zwrt_wlan reload` tears the wiphy down
//!    and re-registers it with a fresh index (phy2 → … → phy9 over one session),
//!    and a temporary vdev does not survive that. So: discover the phy on each
//!    use, never cache it, and never expect a scan vdev to outlive a Wi-Fi
//!    reconfiguration.
//!
//! Nothing here writes uci. That is deliberate — it keeps the detector free of
//! side effects, so the engine can never mistake its own scan for the user
//! having changed something by hand.
//!
//! One scan at a time in a process: the engine's
//! tick and the web's `GET /api/scenario/scan` would otherwise both add and remove `scen-scan0`.

use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};

/// Name for the temporary scan vdev. Distinct from older probe names so a
/// leftover from one cannot be mistaken for the other.
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

/// Longest a single `iw`/`ip` call may take (a two-band scan takes a few seconds): a hung one
/// would otherwise stall the engine's tick for good, heartbeat and all.
const CMD_LIMIT: Duration = Duration::from_secs(20);

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = crate::output_within(Command::new(cmd).args(args), CMD_LIMIT).map_err(|e| format!("{cmd}: {e}"))?;
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
/// of which we want `signal:` and `SSID:`. The BSS line is kept too, so
/// matching can use the BSSID.
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

/// Upper bound for the supplementary passive/DFS 5 GHz scan.
const PASSIVE_LIMIT: Duration = Duration::from_secs(8);

/// Enabled channels in 5150-5730 MHz from `iw phy <phy> info`. The normal full
/// scan never returns networks there on this chip (the regdomain marks them
/// PASSIVE-SCAN / DFS), but an explicit `scan freq …` does (measured 2026-10-27).
pub fn passive_5g_freqs(phy_info: &str) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for line in phy_info.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("* ") else { continue };
        let Some((num, tail)) = rest.split_once(" MHz") else { continue };
        let Ok(f) = num.trim().parse::<f64>() else { continue };
        let f = f as u32;
        if !(5150..=5730).contains(&f) || tail.contains("(disabled)") {
            continue;
        }
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// Arguments for `iw dev <iface> scan freq <f>…`.
pub fn freq_scan_args(iface: &str, freqs: &[u32]) -> Vec<String> {
    let mut a: Vec<String> = vec!["dev".into(), iface.into(), "scan".into(), "freq".into()];
    a.extend(freqs.iter().map(|f| f.to_string()));
    a
}

/// Merge two scan results: one entry per BSSID (case-insensitive), stronger
/// signal wins, strongest first.
pub fn merge_networks(mut a: Vec<Network>, b: Vec<Network>) -> Vec<Network> {
    for n in b {
        match a.iter_mut().find(|x| x.bssid.eq_ignore_ascii_case(&n.bssid)) {
            Some(x) => {
                if n.signal > x.signal {
                    *x = n;
                }
            }
            None => a.push(n),
        }
    }
    a.sort_by(|x, y| y.signal.partial_cmp(&x.signal).unwrap_or(std::cmp::Ordering::Equal));
    a
}

/// Phy of an interface (`wiphy N` → `phyN`), read fresh every time.
fn phy_of(iface: &str) -> Option<String> {
    run("iw", &["dev", iface, "info"]).ok().and_then(|text| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix("wiphy ").map(|n| format!("phy{}", n.trim())))
    })
}

/// Second pass over the passive/DFS 5 GHz channels. Any failure just means
/// "nothing extra".
fn passive_scan(iface: &str) -> Vec<Network> {
    let Some(phy) = phy_of(iface) else { return Vec::new() };
    let Ok(info) = run("iw", &["phy", &phy, "info"]) else { return Vec::new() };
    let freqs = passive_5g_freqs(&info);
    if freqs.is_empty() {
        return Vec::new();
    }
    let args = freq_scan_args(iface, &freqs);
    match crate::output_within(Command::new("iw").args(&args), PASSIVE_LIMIT) {
        Ok(out) if out.status.success() => parse_scan(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}

/// Full scan, then the passive/DFS 5 GHz supplement merged in.
fn scan_full(iface: &str) -> Result<Vec<Network>, String> {
    let nets = scan_on(iface)?;
    Ok(merge_networks(nets, passive_scan(iface)))
}

/// Remove the temporary vdev. Best effort — if it is already gone (a Wi-Fi
/// reload took the whole wiphy with it) that is not an error.
fn drop_scan_iface() {
    let _ = crate::output_within(Command::new("iw").args(["dev", SCAN_IFACE, "del"]), CMD_LIMIT);
}

/// Scan for nearby networks.
///
/// Prefers an interface that already exists; only creates (and always removes)
/// a temporary managed vdev when there is none. Writes no configuration either
/// way.
pub fn scan() -> Result<Vec<Network>, String> {
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    // A leftover from a previous run that died mid-scan would otherwise make
    // `interface add` fail with EEXIST.
    let ifaces = existing_ifaces();
    if ifaces.iter().any(|i| i == SCAN_IFACE) {
        drop_scan_iface();
    }

    if let Some(iface) = existing_ifaces().into_iter().find(|i| i != SCAN_IFACE) {
        if let Ok(nets) = scan_full(&iface) {
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
        scan_full(SCAN_IFACE)
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
	SSID: Cafe?
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
        assert_eq!(nets[0].ssid, "Cafe?");
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

    const PHY_INFO: &str = "Wiphy phy3
\tBand 2:
\t\tFrequencies:
\t\t\t* 5180 MHz [36] (23.0 dBm) (no IR)
\t\t\t* 5200.0 MHz [40] (23.0 dBm) (no IR)
\t\t\t* 5260 MHz [52] (20.0 dBm) (no IR, radar detection)
\t\t\t* 5280 MHz [56] (disabled)
\t\t\t* 5500 MHz [100] (20.0 dBm) (radar detection)
\t\t\t* 5745 MHz [149] (30.0 dBm)
\tBand 1:
\t\t\t* 2412 MHz [1] (20.0 dBm)
";

    #[test]
    fn passive_freqs_skip_disabled_and_out_of_range() {
        assert_eq!(passive_5g_freqs(PHY_INFO), vec![5180, 5200, 5260, 5500]);
        assert_eq!(
            freq_scan_args("wlan0", &[5180, 5260]),
            vec!["dev", "wlan0", "scan", "freq", "5180", "5260"]
        );
    }

    #[test]
    fn merge_dedupes_case_insensitively_keeping_stronger() {
        let n = |b: &str, s: f64| Network { ssid: "x".into(), bssid: b.into(), signal: s };
        let m = merge_networks(
            vec![n("02:00:00:00:00:aa", -70.0), n("02:00:00:00:00:bb", -50.0)],
            vec![n("02:00:00:00:00:AA", -40.0), n("02:00:00:00:00:cc", -90.0)],
        );
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].signal, -40.0);
        assert_eq!(m[1].bssid, "02:00:00:00:00:bb");
        assert_eq!(m[2].signal, -90.0);
    }

    #[test]
    fn missing_signal_defaults_to_very_weak() {
        let nets = parse_scan("BSS 02:00:00:00:00:aa(on x)\n\tSSID: nosignal\n");
        assert_eq!(nets.len(), 1);
        assert_eq!(nets[0].signal, -100.0);
    }
}
