//! The saved power-save choice and the driver reading are separate facts.

use std::fs::File;
use std::io::Read;

pub const HOTPLUG_FILE: &str = "99-disable-powersave";
pub const MAIN_IFACES: [&str; 2] = ["wlan0", "wlan2"];

/// A disabled radio can keep a netdev whose driver still answers `get power_save`.
pub fn main_up() -> [bool; 2] {
    ["/sys/class/net/wlan0/operstate", "/sys/class/net/wlan2/operstate"].map(|path| {
        let mut buf = [0; 16];
        File::open(path)
            .and_then(|mut file| file.read(&mut buf))
            .is_ok_and(|n| buf[..n].trim_ascii() == b"up")
    })
}

fn mode(word: &str) -> Option<bool> {
    match word {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

pub fn from_iw(text: &str) -> Option<bool> {
    let (_, value) = text.lines().find_map(|line| line.split_once("Power save:"))?;
    mode(value.trim())
}

pub fn from_hotplug(text: &str) -> Option<bool> {
    let mut saved = None;
    for line in text.lines() {
        let mut words = line.split_ascii_whitespace();
        let Some(command) = words.next() else { continue };
        if command.rsplit('/').next() == Some("iw")
            && words.next() == Some("dev")
            && words.next().is_some()
            && words.next() == Some("set")
            && words.next() == Some("power_save")
        {
            let value = mode(words.next()?)?;
            if saved.is_some_and(|previous| previous != value) {
                return None;
            }
            saved = Some(value);
        }
    }
    saved
}

pub fn preferred(saved: Option<bool>, live: Option<bool>) -> Option<bool> {
    saved.or(live)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_comments_and_does_not_guess_at_conflicting_saved_modes() {
        let script = "# iw dev wlan0 set power_save off\n  iw dev wlan0 set power_save on 2>/dev/null\n";
        assert_eq!(from_hotplug(script), Some(true));
        assert_eq!(
            from_hotplug(&format!("{script}/usr/sbin/iw dev wlan2 set power_save off\n")),
            None
        );
        assert_eq!(from_iw("Power save: online\n"), None);
    }
}
