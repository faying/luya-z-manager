//! Compare a single wireless snapshot with the netdevs and hostapd it should have produced.

use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::layout::{self, Uci, APS};
use crate::{hostapd_count, hostapd_serves, switches, Host, CMD_TIMEOUT, OFF_DEADLINE, ON_DEADLINE, POLL_EVERY};

// B11 can tear down a briefly working AP more than seven seconds after reporting power-on.
const RELOAD_STABLE: Duration = Duration::from_secs(15);

#[derive(Clone, Copy)]
pub(super) enum Target<'a> {
    Current,
    Aps([bool; 2]),
    Master { wanted: bool, radios_before: [Option<bool>; 2] },
    Written { expected: &'a Uci, switches: switches::Switches, layout: [bool; 4] },
}

impl Target<'_> {
    fn wants_on(self, effective: Option<[bool; 4]>) -> bool {
        match self {
            Self::Current => effective.is_none_or(|on| on.contains(&true)),
            Self::Aps(aps) => aps.contains(&true),
            Self::Master { wanted, .. } => wanted,
            Self::Written { switches, .. } => switches.module == Some(true),
        }
    }

    fn settings_match(self, cfg: &Uci, running: [bool; 4]) -> Option<bool> {
        let switches = || switches::read(|key| cfg.get(key).map(String::as_str));
        match self {
            Self::Current => Some(true),
            Self::Aps(wanted) => {
                let now = switches();
                // A failed vendor-on call can leave a perfectly settled all-down layout. An
                // enable scenario still needs the module to have turned on.
                let actual = [now.main[0]?, now.main[1]?];
                Some(actual == wanted && (now.module? || !wanted.contains(&true)))
            }
            Self::Master { wanted, radios_before } => {
                let now = switches();
                if now.module? != wanted || now.master? != wanted {
                    return Some(false);
                }
                for (before, actual) in radios_before.into_iter().zip(now.radios) {
                    if let Some(before) = before {
                        if actual? != before {
                            return Some(false);
                        }
                    }
                }
                Some(true)
            }
            Self::Written { expected, switches: flags, layout } => {
                let found = switches();
                if layout != running {
                    return Some(false);
                }
                let intended = [flags.master, flags.module, flags.radios[0], flags.radios[1], flags.main[0], flags.main[1]];
                let observed = [found.master, found.module, found.radios[0], found.radios[1], found.main[0], found.main[1]];
                for (wanted, actual) in intended.into_iter().zip(observed) {
                    if wanted.is_some() && wanted != Some(actual?) {
                        return Some(false);
                    }
                }
                for (ap, enabled) in APS.iter().zip(running) {
                    if ap.ifname(cfg)? != ap.ifname(expected)?
                        || (enabled && ap.ssid(cfg) != ap.ssid(expected))
                    {
                        return Some(false);
                    }
                }
                Some(true)
            }
        }
    }

    fn stable_for(self) -> Duration {
        if matches!(self, Self::Written { .. }) { RELOAD_STABLE } else { Duration::ZERO }
    }
}

trait Runtime {
    fn links(&self, ifnames: [&str; 4]) -> [Option<bool>; 4];
    fn serves(&self, ifname: &str, ssid: Option<&str>) -> Option<bool>;
    fn hostapds(&self) -> Option<usize>;
}

struct Device(Option<Instant>);

impl Device {
    fn command_timeout(&self) -> Option<Duration> {
        let timeout = self.0.map_or(CMD_TIMEOUT, |until| {
            CMD_TIMEOUT.min(until.saturating_duration_since(Instant::now()))
        });
        (!timeout.is_zero()).then_some(timeout)
    }
}

impl Runtime for Device {
    fn links(&self, ifnames: [&str; 4]) -> [Option<bool>; 4] {
        links_at(Path::new("/sys/class/net"), ifnames)
    }

    fn serves(&self, ifname: &str, ssid: Option<&str>) -> Option<bool> {
        hostapd_serves(ifname, ssid, self.command_timeout()?)
    }

    fn hostapds(&self) -> Option<usize> {
        hostapd_count(self.command_timeout()?)
    }
}

fn links_at(net: &Path, ifnames: [&str; 4]) -> [Option<bool>; 4] {
    let mut path = PathBuf::with_capacity(net.as_os_str().len() + libc::IFNAMSIZ + "/operstate".len());
    path.push(net);
    let mut missing = [false; 4];
    let mut states = std::array::from_fn(|i| {
        path.push(ifnames[i]);
        path.push("operstate");
        let file = File::open(&path);
        path.pop();
        path.pop();
        match file {
            Ok(mut file) => {
                let mut data = [0; 16];
                let len = file.read(&mut data).ok()?;
                match data[..len].trim_ascii() {
                    b"up" => Some(true),
                    b"down" => Some(false),
                    _ => None,
                }
            }
            Err(e) => {
                missing[i] = e.kind() == ErrorKind::NotFound;
                None
            }
        }
    });
    // A missing operstate only proves down if the whole interface is absent from a readable
    // namespace. Do one enumeration for all missing links, and only trust it if it completed.
    if missing.contains(&true) {
        if let Ok(entries) = fs::read_dir(net) {
            for entry in entries {
                let Ok(entry) = entry else { return states };
                let name = entry.file_name();
                for i in 0..4 {
                    if missing[i] && name == ifnames[i] {
                        missing[i] = false;
                    }
                }
            }
            for i in 0..4 {
                if missing[i] {
                    states[i] = Some(false);
                }
            }
        }
    }
    states
}

fn check_with(cfg: &Uci, target: Target<'_>, on: [bool; 4], r: &impl Runtime) -> Option<bool> {
    if !target.settings_match(cfg, on)? {
        return Some(false);
    }
    let [main_2g, main_5g, guest_2g, guest_5g] = std::array::from_fn(|i| APS[i].ifname(cfg));
    let names = [main_2g?, main_5g?, guest_2g?, guest_5g?];
    for (i, name) in names.iter().enumerate() {
        if names[..i].contains(name) {
            return None;
        }
    }

    // CN B11 can keep hostapd ENABLED after the vendor lowers the netdev. Require positive
    // link state in both directions, then check hostapd and SSID for each running main AP.
    for (actual, expected) in r.links(names).into_iter().zip(on) {
        if actual? != expected {
            return Some(false);
        }
    }
    for i in 0..2 {
        if on[i] && !r.serves(names[i], APS[i].ssid(cfg))? {
            return Some(false);
        }
    }

    if on.contains(&true) {
        Some(true)
    } else {
        Some(r.hostapds()? == 0)
    }
}

pub(super) fn check(cfg: &Uci, target: Target<'_>, on: [bool; 4], until: Option<Instant>) -> Option<bool> {
    check_with(cfg, target, on, &Device(until))
}

pub(super) fn fully_on(cfg: &Uci) -> bool {
    fully_on_with(cfg, &Device(None))
}

fn fully_on_with(cfg: &Uci, r: &impl Runtime) -> bool {
    let Some(on) = layout::running_checked(cfg) else { return false };
    (on[0] || on[1]) && check_with(cfg, Target::Aps([true; 2]), on, r) == Some(true)
}

pub(super) fn poll(h: &dyn Host, target: Target<'_>) -> (Option<bool>, Option<Uci>) {
    let start = Instant::now();
    let mut last_known = None;
    let mut matching_since = None;
    loop {
        let cfg = h.wireless();
        let on = cfg.as_ref().and_then(layout::running_checked);
        last_known = on.or(last_known);
        let until = start + if target.wants_on(last_known) { ON_DEADLINE } else { OFF_DEADLINE };
        let result = cfg.as_ref().zip(on).and_then(|(cfg, on)| check(cfg, target, on, Some(until)));
        let now = Instant::now();
        if now <= until && has_settled(&mut matching_since, now - start, target.stable_for(), result) {
            return (Some(true), cfg);
        }
        let left = until.saturating_duration_since(now);
        if left <= POLL_EVERY {
            return (result.map(|_| false), cfg);
        }
        std::thread::sleep(POLL_EVERY);
    }
}

fn has_settled(
    since: &mut Option<Duration>, now: Duration, minimum: Duration, found: Option<bool>,
) -> bool {
    if found == Some(true) {
        now.saturating_sub(*since.get_or_insert(now)) >= minimum
    } else {
        *since = None;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{status_serves, AP_2G, AP_5G, VENDOR_ONOFF};

    const USER: &str = "wireless.zte_mbb.wifi_onoff_by_user";
    const RADIO_5G: &str = "wireless.wifi1.disabled";
    const IFNAMES: [&str; 4] = ["wlan0", "wlan2", "wlan1", "wlan3"];

    fn five_only() -> Uci {
        [
            (VENDOR_ONOFF, "1"),
            (USER, "1"),
            ("wireless.wifi0", "wifi-device"),
            ("wireless.wifi0.disabled", "1"),
            ("wireless.wifi1", "wifi-device"),
            (RADIO_5G, "0"),
            ("wireless.main_2g", "wifi-iface"),
            (AP_2G, "0"),
            ("wireless.main_5g", "wifi-iface"),
            (AP_5G, "0"),
            ("wireless.main_5g.ssid", "home5"),
        ].into_iter().map(|(k, v)| (k.into(), v.into())).collect()
    }

    struct Sample {
        links: [Option<bool>; 4],
        status: Option<&'static str>,
        processes: Option<usize>,
    }

    impl Runtime for Sample {
        fn links(&self, names: [&str; 4]) -> [Option<bool>; 4] {
            assert_eq!(names, IFNAMES);
            self.links
        }

        fn serves(&self, name: &str, ssid: Option<&str>) -> Option<bool> {
            assert_eq!(name, "wlan2");
            assert_eq!(ssid, Some("home5"));
            self.status.map(|status| status_serves(status, ssid))
        }

        fn hostapds(&self) -> Option<usize> {
            self.processes
        }
    }

    fn sample_result(cfg: &Uci, target: Target<'_>, r: &Sample) -> Option<bool> {
        check_with(cfg, target, layout::running_checked(cfg)?, r)
    }

    #[test]
    fn single_band_scenario_requires_actual_layout() {
        let mut cfg = five_only();
        cfg.insert(USER.into(), "0".into()); // the module is already working
        let mut r = Sample {
            links: [Some(false), Some(true), Some(false), Some(false)],
            status: Some("state=ENABLED\nssid[0]=home5\n"),
            processes: None,
        };
        let both = Target::Aps([true, true]);
        assert_eq!(sample_result(&cfg, both, &r), Some(true));
        assert!(fully_on_with(&cfg, &r));

        r.links[1] = Some(false); // hostapd still answers ENABLED after the netdev is down
        assert_eq!(sample_result(&cfg, both, &r), Some(false));
        assert!(!fully_on_with(&cfg, &r));
        r.links[1] = Some(true);
        r.links[0] = Some(true); // 2.4 GHz has not stopped broadcasting yet
        assert_eq!(sample_result(&cfg, both, &r), Some(false));
        r.links[0] = None;
        assert_eq!(sample_result(&cfg, both, &r), None);
        r.links[0] = Some(false);

        cfg.insert(AP_2G.into(), "1".into()); // no runtime change, but scenario requested the flag on
        assert_eq!(sample_result(&cfg, Target::Current, &r), Some(true));
        assert_eq!(sample_result(&cfg, both, &r), Some(false));
        cfg.insert(AP_2G.into(), "0".into());
        cfg.insert(VENDOR_ONOFF.into(), "0".into()); // failed vendor-on
        r.links[1] = Some(false);
        r.processes = Some(0);
        assert_eq!(sample_result(&cfg, Target::Current, &r), Some(true));
        assert_eq!(sample_result(&cfg, both, &r), Some(false));
        assert!(!fully_on_with(&cfg, &r));
    }

    #[test]
    fn master_waits_for_both_flags_saved_bands_and_beacons() {
        let mut cfg = five_only();
        let mut r = Sample { links: [Some(false); 4], status: None, processes: Some(0) };
        let master = |wanted| Target::Master { wanted, radios_before: [Some(false), Some(true)] };

        cfg.insert(VENDOR_ONOFF.into(), "0".into());
        cfg.insert(RADIO_5G.into(), "1".into());
        cfg.insert("wireless.wifi0.disabled_bk".into(), "1".into());
        assert_eq!(sample_result(&cfg, master(false), &r), Some(false)); // user bit still on
        cfg.insert(USER.into(), "0".into());
        assert_eq!(sample_result(&cfg, master(false), &r), Some(true));

        cfg.insert(USER.into(), "1".into());
        cfg.insert(VENDOR_ONOFF.into(), "1".into());
        // Module up, but the vendor has not restored raw radio flags yet.
        assert_eq!(sample_result(&cfg, master(true), &r), Some(false));
        cfg.insert(RADIO_5G.into(), "0".into());
        r.links[1] = Some(true);
        assert_eq!(sample_result(&cfg, master(true), &r), None); // no hostapd response
        r.status = Some("state=ENABLED\nssid[0]=old5\n");
        assert_eq!(sample_result(&cfg, master(true), &r), Some(false));
        r.status = Some("state=ENABLED\nssid[0]=home5\n");
        assert_eq!(sample_result(&cfg, master(true), &r), Some(true));
    }

    #[test]
    fn reload_rejects_a_late_teardown_and_never_adopts_the_new_module_state() {
        let wanted = five_only();
        let target = Target::Written {
            expected: &wanted,
            switches: switches::read(|k| wanted.get(k).map(String::as_str)),
            layout: layout::running_checked(&wanted).unwrap(),
        };
        let mut sample = Sample {
            links: [Some(false), Some(true), Some(false), Some(false)],
            status: Some("state=ENABLED\nssid[0]=home5\n"),
            processes: Some(0),
        };
        let mut since = None;
        let update = |since: &mut Option<Duration>, t, cfg: &Uci, sample: &Sample| {
            has_settled(since, Duration::from_secs(t), target.stable_for(), sample_result(cfg, target, sample))
        };
        assert!(!update(&mut since, 2, &wanted, &sample));
        assert!(!update(&mut since, 9, &wanted, &sample));
        sample.links[1] = Some(false);
        assert!(!update(&mut since, 10, &wanted, &sample));

        let mut stopped = wanted.clone();
        stopped.insert(VENDOR_ONOFF.into(), "0".into());
        stopped.insert(RADIO_5G.into(), "1".into());
        assert_eq!(sample_result(&stopped, Target::Current, &sample), Some(true));
        assert_eq!(sample_result(&stopped, target, &sample), Some(false));
        assert!(!update(&mut since, 15, &stopped, &sample));

        let mut renamed = wanted.clone();
        renamed.insert("wireless.main_5g.ssid".into(), "old5".into());
        assert_eq!(sample_result(&renamed, target, &sample), Some(false));
        sample.links[1] = Some(true);
        assert!(!update(&mut since, 17, &wanted, &sample));
        sample.status = None;
        assert!(!update(&mut since, 22, &wanted, &sample));
        sample.status = Some("state=ENABLED\nssid[0]=home5\n");
        assert!(!update(&mut since, 23, &wanted, &sample));
        assert!(!update(&mut since, 37, &wanted, &sample));
        assert!(update(&mut since, 38, &wanted, &sample));
    }

    #[test]
    fn reload_waits_past_an_empty_layout_before_the_module_drops() {
        let mut wanted = five_only();
        wanted.insert(RADIO_5G.into(), "1".into());
        let target = Target::Written {
            expected: &wanted,
            switches: switches::read(|k| wanted.get(k).map(String::as_str)),
            layout: layout::running_checked(&wanted).unwrap(),
        };
        let sample = Sample { links: [Some(false); 4], status: None, processes: Some(0) };
        let mut since = None;
        assert!(!has_settled(&mut since, Duration::from_secs(3), target.stable_for(), sample_result(&wanted, target, &sample)));

        let mut stopped = wanted.clone();
        stopped.insert(VENDOR_ONOFF.into(), "0".into());
        assert_eq!(sample_result(&stopped, Target::Current, &sample), Some(true));
        assert_eq!(sample_result(&stopped, target, &sample), Some(false));
        assert!(!has_settled(&mut since, Duration::from_secs(19), target.stable_for(), sample_result(&stopped, target, &sample)));
    }

    #[test]
    fn unknown_config_and_empty_layout_require_positive_evidence() {
        let mut cfg = five_only();
        let mut r = Sample { links: [Some(false); 4], status: None, processes: None };
        cfg.remove(VENDOR_ONOFF);
        assert_eq!(sample_result(&cfg, Target::Current, &r), None);
        cfg.insert(VENDOR_ONOFF.into(), "1".into());
        cfg.insert(RADIO_5G.into(), "invalid".into());
        assert_eq!(sample_result(&cfg, Target::Current, &r), None);
        cfg.remove("wireless.wifi1");
        cfg.remove(RADIO_5G);
        assert_eq!(sample_result(&cfg, Target::Current, &r), None);

        cfg.insert(AP_5G.into(), "1".into()); // radio no longer matters, all APs blocked
        assert_eq!(sample_result(&cfg, Target::Current, &r), None); // process scan unavailable
        r.processes = Some(1);
        assert_eq!(sample_result(&cfg, Target::Current, &r), Some(false));
        r.processes = Some(0);
        assert_eq!(sample_result(&cfg, Target::Current, &r), Some(true));
        cfg.insert(VENDOR_ONOFF.into(), "0".into());
        r.links[2] = Some(true);
        assert_eq!(sample_result(&cfg, Target::Current, &r), Some(false));
    }

    #[test]
    fn guest_eligibility_uses_the_real_radio_and_link() {
        let mut cfg = five_only();
        cfg.insert("wireless.guest_2g".into(), "wifi-iface".into());
        cfg.insert("wireless.guest_5g".into(), "wifi-iface".into());
        let mut r = Sample {
            links: [Some(false), Some(true), Some(false), Some(false)],
            status: Some("state=ENABLED\nssid[0]=home5\n"),
            processes: Some(1),
        };
        assert_eq!(sample_result(&cfg, Target::Current, &r), Some(false));
        r.links[3] = Some(true);
        assert_eq!(sample_result(&cfg, Target::Current, &r), Some(true));
        assert_eq!(layout::ifname(&cfg, "guest_5g"), Some("wlan3"));
        cfg.insert("wireless.guest_5g.ifname".into(), "g5".into());
        assert_eq!(layout::ifname(&cfg, "guest_5g"), Some("g5"));
    }

    #[test]
    fn sysfs_only_proves_down_for_explicit_down_or_verified_absence() {
        let net = std::env::temp_dir().join(format!("luyaz-wifi-net-{}", std::process::id()));
        assert_eq!(links_at(&net, IFNAMES), [None; 4]);
        fs::create_dir_all(net.join("wlan0")).unwrap();
        fs::create_dir_all(net.join("wlan2")).unwrap();
        fs::create_dir_all(net.join("wlan3")).unwrap();
        fs::write(net.join("wlan0/operstate"), "down\n").unwrap();
        fs::write(net.join("wlan2/operstate"), "unknown\n").unwrap();
        // wlan1 is absent; wlan3 exists but its operstate cannot be read.
        assert_eq!(links_at(&net, IFNAMES), [Some(false), None, Some(false), None]);
        fs::write(net.join("wlan3/operstate"), "up\n").unwrap();
        assert_eq!(links_at(&net, IFNAMES)[3], Some(true));
        fs::remove_dir_all(&net).unwrap();
    }
}
