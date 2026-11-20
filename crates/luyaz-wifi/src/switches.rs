//! Saved Wi-Fi choices and the separate vendor module switch from `uci show wireless`.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Switches {
    pub master: Option<bool>,
    pub module: Option<bool>,
    pub radios: [Option<bool>; 2],
    pub main: [Option<bool>; 2],
}

const KEYS: [&str; 12] = [
    "wireless.zte_mbb.wifi_onoff_by_user",
    "wireless.zte_mbb.wifi_onoff",
    "wireless.wifi0",
    "wireless.wifi0.disabled",
    "wireless.wifi0.disabled_bk",
    "wireless.main_2g",
    "wireless.main_2g.disabled",
    "wireless.wifi1",
    "wireless.wifi1.disabled",
    "wireless.wifi1.disabled_bk",
    "wireless.main_5g",
    "wireless.main_5g.disabled",
];

pub fn read<'a>(get: impl Fn(&str) -> Option<&'a str>) -> Switches {
    parse(KEYS.map(get))
}

pub fn read_show(text: &str) -> Switches {
    let mut values = [None; KEYS.len()];
    for (key, value) in text.lines().filter_map(|line| line.split_once('=')) {
        if let Some(i) = KEYS.iter().position(|k| *k == key) {
            let value = value
                .strip_prefix('\'')
                .and_then(|v| v.strip_suffix('\''))
                .unwrap_or(value);
            values[i] = Some(value);
        }
    }
    parse(values)
}

fn bit(value: Option<&str>) -> Option<bool> {
    match value? {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn enabled(disabled: Option<&str>) -> Option<bool> {
    bit(Some(disabled.unwrap_or("0"))).map(|v| !v)
}

fn parse([user, module, r2, d2, b2, a2, ad2, r5, d5, b5, a5, ad5]: [Option<&str>; 12]) -> Switches {
    let module = bit(module);
    let saved_radio = |section: Option<&str>, disabled, backup| {
        section?;
        let raw = enabled(disabled)?;
        // CN B11 forces disabled=1 at module-off, saving a previously disabled radio in disabled_bk.
        if module == Some(false) && !raw {
            enabled(backup)
        } else {
            Some(raw)
        }
    };
    Switches {
        master: bit(user).or(module),
        module,
        radios: [saved_radio(r2, d2, b2), saved_radio(r5, d5, b5)],
        main: [a2.and_then(|_| enabled(ad2)), a5.and_then(|_| enabled(ad5))],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_bands_survive_module_off() {
        let config = "wireless.wifi0=wifi-device\nwireless.wifi1=wifi-device\n\
            wireless.wifi0.disabled='1'\nwireless.wifi1.disabled='1'\n\
            wireless.wifi0.disabled_bk='1'\nwireless.zte_mbb.wifi_onoff='0'\n\
            wireless.zte_mbb.wifi_onoff_by_user='1'\n";
        let off = read_show(config);
        assert_eq!((off.master, off.module), (Some(true), Some(false)));
        assert_eq!(off.radios, [Some(false), Some(true)]);
        let starting = read_show(&config.replace("wifi_onoff='0'", "wifi_onoff='1'"));
        assert_eq!(starting.radios, [Some(false), Some(false)]);
    }

    #[test]
    fn invalid_flags_are_not_defaults_and_missing_sections_stay_unknown() {
        let rows = [
            ("wireless.zte_mbb.wifi_onoff_by_user", "invalid"),
            ("wireless.zte_mbb.wifi_onoff", "0"),
            ("wireless.wifi0", "wifi-device"),
            ("wireless.wifi0.disabled", "1"),
            ("wireless.wifi0.disabled_bk", "invalid"),
            ("wireless.wifi1.disabled", "0"),
            ("wireless.main_2g", "wifi-iface"),
            ("wireless.main_5g", "wifi-iface"),
            ("wireless.main_5g.disabled", "invalid"),
        ];
        let state = read(|key| rows.iter().find(|(k, _)| *k == key).map(|(_, v)| *v));
        assert_eq!(state.master, Some(false));
        assert_eq!(state.radios, [None, None]);
        assert_eq!(state.main, [Some(true), None]);
    }
}
