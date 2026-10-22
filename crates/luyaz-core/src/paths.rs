//! The core's state is under luyaz names on every device (`/data/luyaz/core`, `/tmp/luyaz`,
//! `/data/luyaz/screen`); callers use those paths directly. The install kit and the ship executor
//! refuse a device with an older layout.

pub const MANIFEST: &str = "/data/luyaz/ship/manifest.jsonl";

/// A process running the ship executor, by its cmdline.
pub fn is_ship_cmdline(cmdline: &str) -> bool {
    cmdline.contains("luyaz-ship.sh")
}

/// The core's program name in ledger and report keys.
pub const CORE: &str = "luyaz-core";

/// A process or file name that is the core: `luyaz-core*` (incl. the side-by-side
/// `luyaz-core.test`). By prefix, never exact (9-17).
pub fn is_core(name: &str) -> bool {
    name.starts_with("luyaz-core")
}

/// The touch UI's program name in ledger and report keys.
pub const UI: &str = "luyaz-ui";

/// A comm that is our touch UI: `luyaz-ui*` (side-by-side builds included). By prefix, never
/// exact (9-17).
pub fn is_ui(name: &str) -> bool {
    name.starts_with("luyaz-ui")
}

/// `name` as a ledger key: any core name (side-by-side included) becomes [`CORE`]; others stay.
pub fn prog_key(name: &str) -> &str {
    if is_core(name) {
        CORE
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_names_by_prefix() {
        for n in [
            "luyaz-core",
            "luyaz-core.test",
        ] {
            assert!(is_core(n), "{n}");
        }
        for n in [
            "zwrt-datad",
            "luyaz-sentinel",
            "luyaz-cor",
            "",
        ] {
            assert!(!is_core(n), "{n}");
        }
    }

    #[test]
    fn ui_names_by_prefix() {
        for n in ["luyaz-ui", "luyaz-ui.test"] {
            assert!(is_ui(n), "{n}");
        }
        for n in [
            "zte_topsw_devui",
            "luyaz-u",
            "luyaz-core",
            "",
        ] {
            assert!(!is_ui(n), "{n}");
        }
        assert_eq!(prog_key("luyaz-ui"), UI);
        assert_eq!(prog_key("luyaz-ui.te"), "luyaz-ui.te");
        assert_eq!(prog_key("luyaz-core.test"), CORE);
    }

    #[test]
    fn ship_cmdline() {
        assert!(!is_ship_cmdline("sh\0/data/u60-guard/u60-ship.sh\0run"));
        assert!(is_ship_cmdline("sh /data/luyaz/scripts/luyaz-ship.sh run"));
        assert!(!is_ship_cmdline("sh /data/luyaz/scripts/doctor.sh"));
    }
}
