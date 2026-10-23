//! Which AP layouts the firmware cannot run, checked on the `wireless` uci a write would leave
//! behind. Pure: callers pass the uci before and after (`uci show wireless` keys).
//!
//! Three layouts take all Wi-Fi down on the U60 Pro, and the device may be reachable only over it:
//!
//!  * **No 2.4 GHz AP while a 5 GHz one runs** (found on MU5250): the stock QCMAP then
//!    starts single-AP mode, which only brings an AP up on wlan0; 5 GHz lives on wlan2, so hostapd
//!    never gets ready. Turning off only 5 GHz, or both, is fine.
//!  * **A guest AP without its band's main AP**: the main AP is the band's first interface (wlan0 /
//!    wlan2); the guest one is stacked on it.
//!  * **Exactly three APs** (upstream #181, measured on MU5250): QCMAP supports 1, 2 or 4; with 3 it
//!    starts in `mode=4`, reports it unsupported and Wi-Fi stays down. Checked on every model — no
//!    caller needs three.
//!
//! Upstream remaps interface names (`wlanx`) instead; we refuse, because wlan0 = 2.4 GHz and
//! wlan2 = 5 GHz are written into many places here. A write that leaves the running set as it was
//! is never refused (a device already in a bad layout can still be renamed, or repaired).

use std::collections::BTreeMap;

pub type Uci = BTreeMap<String, String>;

/// AP sections, with the band and the radio they sit on when `device` is not set.
const APS: [(&str, Band, &str); 4] = [
    ("main_2g", Band::G2, "wifi0"),
    ("main_5g", Band::G5, "wifi1"),
    ("guest_2g", Band::G2, "wifi0"),
    ("guest_5g", Band::G5, "wifi1"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Band {
    G2,
    G5,
}

pub const NO_2G: &str = "turning 2.4 GHz off while 5 GHz stays on stops all Wi-Fi on this device; turn both off, or only 5 GHz";
pub const GUEST_ALONE: &str = "a guest network needs the main network of its band on";
pub const THREE_APS: &str = "this device cannot run exactly three Wi-Fi networks; turn the guest network on for both bands, or off";

fn get<'a>(m: &'a Uci, key: &str) -> &'a str {
    m.get(key).map(String::as_str).unwrap_or("")
}

/// The AP sections that would broadcast: the section exists, it and its radio are not disabled, and
/// the vendor master switch is not off.
pub fn running(m: &Uci) -> Vec<&'static str> {
    if get(m, "wireless.zte_mbb.wifi_onoff") == "0" {
        return Vec::new();
    }
    APS.iter()
        .filter(|(sec, _, radio)| {
            let device = match get(m, &format!("wireless.{sec}.device")) {
                "" => *radio,
                d => d,
            };
            m.contains_key(&format!("wireless.{sec}"))
                && get(m, &format!("wireless.{sec}.disabled")) != "1"
                && get(m, &format!("wireless.{device}.disabled")) != "1"
        })
        .map(|(sec, ..)| *sec)
        .collect()
}

/// Why the layout in `after` must not be written, or None. Only checked when the running set changes.
pub fn refusal(before: &Uci, after: &Uci) -> Option<&'static str> {
    let on = running(after);
    if on == running(before) {
        return None;
    }
    let on = on.as_slice();
    let band = |b: Band| APS.iter().any(|(s, x, _)| *x == b && on.contains(s));
    if band(Band::G5) && !band(Band::G2) {
        return Some(NO_2G);
    }
    if (on.contains(&"guest_2g") && !on.contains(&"main_2g"))
        || (on.contains(&"guest_5g") && !on.contains(&"main_5g"))
    {
        return Some(GUEST_ALONE);
    }
    (on.len() == 3).then_some(THREE_APS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uci(main_2g: bool, main_5g: bool, guest_2g: bool, guest_5g: bool) -> Uci {
        let mut m = Uci::new();
        for (sec, on) in [
            ("main_2g", main_2g),
            ("main_5g", main_5g),
            ("guest_2g", guest_2g),
            ("guest_5g", guest_5g),
        ] {
            m.insert(format!("wireless.{sec}"), "wifi-iface".into());
            m.insert(
                format!("wireless.{sec}.disabled"),
                if on { "0" } else { "1" }.into(),
            );
        }
        for r in ["wifi0", "wifi1"] {
            m.insert(format!("wireless.{r}"), "wifi-device".into());
            m.insert(format!("wireless.{r}.disabled"), "0".into());
        }
        m.insert("wireless.zte_mbb.wifi_onoff".into(), "1".into());
        m
    }

    /// All 16 combinations of main 2.4/5 × guest 2.4/5, written from "everything off".
    #[test]
    fn truth_table() {
        let off = uci(false, false, false, false);
        for bits in 0..16u8 {
            let [m2, m5, g2, g5] = [bits & 8 != 0, bits & 4 != 0, bits & 2 != 0, bits & 1 != 0];
            let want = if !(m2 || g2) && (m5 || g5) {
                Some(NO_2G)
            } else if (g2 && !m2) || (g5 && !m5) {
                Some(GUEST_ALONE)
            } else if [m2, m5, g2, g5].iter().filter(|b| **b).count() == 3 {
                Some(THREE_APS)
            } else {
                None
            };
            assert_eq!(
                refusal(&off, &uci(m2, m5, g2, g5)),
                want,
                "main {m2}/{m5} guest {g2}/{g5}"
            );
        }
        // what is allowed: nothing, 2.4 only, both mains, all four
        for ok in [
            (false, false, false, false),
            (true, false, false, false),
            (true, true, false, false),
            (true, true, true, true),
        ] {
            assert_eq!(refusal(&off, &uci(ok.0, ok.1, ok.2, ok.3)), None);
        }
        assert_eq!(
            refusal(&off, &uci(true, false, true, false)),
            None,
            "2.4 main + 2.4 guest = two"
        );
    }

    #[test]
    fn radio_off_counts_as_its_aps_off() {
        let before = uci(true, true, false, false);
        let mut after = before.clone();
        after.insert("wireless.wifi0.disabled".into(), "1".into());
        assert_eq!(
            refusal(&before, &after),
            Some(NO_2G),
            "radio2_disabled=1 with 5G on"
        );
        after.insert("wireless.wifi1.disabled".into(), "1".into());
        assert_eq!(refusal(&before, &after), None, "both radios off");
        let mut only5off = before.clone();
        only5off.insert("wireless.wifi1.disabled".into(), "1".into());
        assert_eq!(refusal(&before, &only5off), None);
    }

    #[test]
    fn radio_from_device_option() {
        let before = uci(true, true, false, false);
        let mut after = before.clone();
        after.insert("wireless.main_2g.device".into(), "radio0".into());
        after.insert("wireless.radio0.disabled".into(), "1".into());
        assert_eq!(refusal(&before, &after), Some(NO_2G));
    }

    #[test]
    fn master_off_runs_nothing() {
        let before = uci(true, true, false, false);
        let mut after = uci(false, true, false, false);
        after.insert("wireless.zte_mbb.wifi_onoff".into(), "0".into());
        assert!(running(&after).is_empty());
        assert_eq!(refusal(&before, &after), None);
    }

    #[test]
    fn missing_guest_section_does_not_run() {
        let off = uci(false, false, false, false);
        let mut after = uci(true, true, true, true);
        after.remove("wireless.guest_5g");
        assert_eq!(refusal(&off, &after), Some(THREE_APS));
    }

    #[test]
    fn unchanged_layout_is_never_refused() {
        let bad = uci(false, true, false, false);
        let mut renamed = bad.clone();
        renamed.insert("wireless.main_5g.ssid".into(), "x".into());
        assert_eq!(refusal(&bad, &renamed), None);
        // and a repair out of it is allowed
        assert_eq!(refusal(&bad, &uci(true, true, false, false)), None);
    }
}
