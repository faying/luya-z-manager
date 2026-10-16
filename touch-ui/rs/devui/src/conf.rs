//! devui.conf (src/ui.c load_devui_conf/save_devui_conf): speed_bits, autooff, bright,
//! appearance*, lang, usb_attach. Saving writes exactly these keys.
// SPDX-License-Identifier: MIT

use crate::clock;
use crate::lang;
use crate::logic::{self, Appear, UsbPref};

/// Luya-Z D5: with the program (it was in /data/plugins/u60pro-devui)
pub const CONF_FILE: &str = "/data/luyaz/ui/devui.conf";

#[derive(Clone, Debug, PartialEq)]
pub struct Conf {
    pub speed_bits: bool,
    pub autooff_ms: i32,
    pub bright: i32,
    pub appear: Appear,
    pub dark_from: String,
    pub dark_to: String,
    pub lang_en: bool,
    pub usb: UsbPref,
    /// where save() writes; None = nowhere (the offscreen harness)
    pub path: Option<String>,
}

impl Default for Conf {
    fn default() -> Self {
        Conf {
            speed_bits: true,
            autooff_ms: 60000,
            bright: 232,
            appear: Appear::Light,
            dark_from: "19:00".into(),
            dark_to: "07:00".into(),
            lang_en: false,
            usb: UsbPref::Ask,
            path: None,
        }
    }
}

/// sscanf "%d": optional blanks and sign, digits; the rest of the line is ignored.
fn int_prefix(s: &str) -> Option<i32> {
    let s = s.trim_start();
    let (sign, digits) = match s.as_bytes().first() {
        Some(b'-') => (-1, &s[1..]),
        Some(b'+') => (1, &s[1..]),
        _ => (1, s),
    };
    let n: String = digits.chars().take_while(|c| c.is_ascii_digit()).collect();
    n.parse::<i64>().ok().map(|v| (sign * v) as i32)
}

/// sscanf "%Ns": the first word, at most n bytes.
fn word(s: &str, n: usize) -> Option<&str> {
    let w = s.split_ascii_whitespace().next()?;
    let mut e = w.len().min(n);
    while !w.is_char_boundary(e) {
        e -= 1;
    }
    Some(&w[..e])
}

impl Conf {
    /// Read the file's text over the defaults; unknown lines are ignored, odd values reported.
    pub fn parse(text: &str) -> Conf {
        let mut c = Conf::default();
        for line in text.split_inclusive('\n') {
            // fgets with a 64-byte buffer: longer lines are read in pieces
            if let Some(v) = line.strip_prefix("speed_bits=").and_then(int_prefix) {
                c.speed_bits = v != 0;
            } else if let Some(v) = line.strip_prefix("autooff=").and_then(int_prefix) {
                c.autooff_ms = v;
            } else if let Some(v) = line.strip_prefix("bright=").and_then(int_prefix) {
                c.bright = v;
            } else if let Some(v) = line.strip_prefix("appearance=").and_then(|s| word(s, 15)) {
                let (a, ok) = logic::appear_parse(Some(v));
                if !ok {
                    eprintln!("ui: devui.conf appearance={v} not understood, using light");
                }
                c.appear = a;
            } else if let Some(v) = line.strip_prefix("appearance_dark_from=").and_then(|s| word(s, 7)) {
                c.dark_from = v.into();
            } else if let Some(v) = line.strip_prefix("appearance_dark_to=").and_then(|s| word(s, 7)) {
                c.dark_to = v.into();
            } else if let Some(v) = line.strip_prefix("lang=") {
                c.lang_en = lang::parse(v);
            } else if let Some(v) = line.strip_prefix("usb_attach=") {
                match logic::usb_pref_parse(v) {
                    Some(p) => c.usb = p,
                    None => eprintln!("ui: devui.conf {} not understood, asking", line.trim_end()),
                }
            }
        }
        c
    }

    /// The file, or the defaults when it cannot be read.
    pub fn load(path: &str) -> Conf {
        let mut c = std::fs::read_to_string(path).map(|s| Conf::parse(&s)).unwrap_or_default();
        c.path = Some(path.into());
        c
    }

    pub fn text(&self) -> String {
        format!(
            "speed_bits={}\nautooff={}\nbright={}\nappearance={}\nappearance_dark_from={}\nappearance_dark_to={}\nlang={}\nusb_attach={}\n",
            self.speed_bits as i32,
            self.autooff_ms,
            self.bright,
            logic::appear_name(self.appear),
            self.dark_from,
            self.dark_to,
            if self.lang_en { "en" } else { "zh" },
            logic::usb_pref_name(self.usb)
        )
    }

    pub fn save(&self) {
        if let Some(p) = &self.path
            && let Err(e) = std::fs::write(p, self.text())
        {
            eprintln!("ui: cannot write {p}: {e}");
        }
    }

    /// Dark now? (appear_resolve: the device-local minute, -1 while the clock is unsynced)
    pub fn dark_now(&self, wall: i64) -> bool {
        let min = if logic::clock_sane(wall) {
            let tm = clock::local(wall);
            tm.hour * 60 + tm.min
        } else {
            -1
        };
        logic::is_dark(self.appear, min, logic::hhmm_parse(Some(&self.dark_from)), logic::hhmm_parse(Some(&self.dark_to)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_c_ui_reads() {
        let c = Conf::parse("appearance=dark\nautooff=0\nlang=en\n");
        assert!(c.appear == Appear::Dark && c.autooff_ms == 0 && c.lang_en && c.speed_bits);
        let c = Conf::parse("speed_bits=0\nbright=120\nappearance=Dark\nusb_attach=share\nappearance_dark_from=20:30\ntheme=0\n");
        assert!(!c.speed_bits && c.bright == 120 && c.appear == Appear::Light && c.usb == UsbPref::Share && c.dark_from == "20:30");
        assert!(!Conf::parse("lang=zh\n").lang_en);
        assert_eq!(Conf::parse("").autooff_ms, 60000);
    }

    #[test]
    fn writes_every_key_and_reads_it_back() {
        let c = Conf { appear: Appear::Auto, lang_en: true, usb: UsbPref::Fast, autooff_ms: 30000, ..Conf::default() };
        let back = Conf::parse(&c.text());
        assert_eq!(back, c);
        assert_eq!(
            c.text(),
            "speed_bits=1\nautooff=30000\nbright=232\nappearance=auto\nappearance_dark_from=19:00\nappearance_dark_to=07:00\nlang=en\nusb_attach=fast_charge\n"
        );
    }

    #[test]
    fn dark_by_appearance() {
        // 2026-09-24 11:41:52 device-local (the render test's clock)
        let noon = 1790250112;
        assert!(!Conf::default().dark_now(noon));
        assert!(Conf { appear: Appear::Dark, ..Conf::default() }.dark_now(noon));
        assert!(!Conf { appear: Appear::Auto, ..Conf::default() }.dark_now(noon));
        assert!(Conf { appear: Appear::Auto, dark_from: "11:00".into(), ..Conf::default() }.dark_now(noon));
    }
}
