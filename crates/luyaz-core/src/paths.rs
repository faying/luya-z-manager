//! Phase 10 (manager docs/designs/u60-phase10.md): the core's state is under luyaz names on every
//! device (`/data/luyaz/core`, `/tmp/luyaz`, `/data/luyaz/screen`); callers use those paths directly.
//! Luya-Z rename E3 removed the old-to-new mapping (`/data/u60d`, `/tmp/u60-guard`,
//! `/tmp/u60-guard.handover`, `/data/u60-uid`): the install kit writes only the new layout (E1)
//! and refuses an old one, and so does the ship executor.
//!
//! What is left is the core layout's switch, [`live`] (the sh `luyaz_new` in touch-ui scripts):
//! doctor still reports a move that stopped half-way.

use std::path::Path;

pub const MARK: &str = ".migrated";
pub const DATA_NEW: &str = "/data/luyaz/core";
pub const MANIFEST: &str = "/data/luyaz/ship/manifest.jsonl";

/// A process running the ship executor, by its cmdline (either name; D4).
pub fn is_ship_cmdline(cmdline: &str) -> bool {
    cmdline.contains("u60-ship.sh") || cmdline.contains("luyaz-ship.sh")
}

/// Which directory of a tree is live under `root` (`""` on the device, a test root otherwise).
#[derive(Debug, PartialEq, Eq)]
pub enum Live {
    Old,
    New,
    /// the new directory is there without the mark: the old one is live
    HalfMoved,
}

/// The core's program name in ledger and report keys (old records use it; 10e keeps it).
pub const CORE: &str = "zwrt-datad";

/// A process or file name that is the core: `zwrt-datad*` (incl. the side-by-side
/// `zwrt-datad.test`) or the Phase 10 name `luyaz-core*`. By prefix, never exact (9-17).
pub fn is_core(name: &str) -> bool {
    name.starts_with("zwrt-datad") || name.starts_with("luyaz-core")
}

/// `name` as a ledger key: any core name becomes [`CORE`], others stay.
pub fn core_key(name: &str) -> &str {
    if is_core(name) { CORE } else { name }
}

/// The touch UI's program name in ledger and report keys (10h R5 keeps the old one).
pub const UI: &str = "u60pro-devui";

/// A comm that is our touch UI: `luyaz-ui*` (side-by-side builds included). By prefix, never
/// exact (9-17). The old name u60pro-devui is gone since Phase 10 R5 round ③.
pub fn is_ui(name: &str) -> bool {
    name.starts_with("luyaz-ui")
}

/// `name` as a ledger key: any core name becomes [`CORE`], the new UI name `luyaz-ui` becomes
/// [`UI`] (exactly that name, as `u60pro-devui` itself was matched exactly); others stay.
pub fn prog_key(name: &str) -> &str {
    if is_core(name) {
        CORE
    } else if name == "luyaz-ui" {
        UI
    } else {
        name
    }
}

pub fn live(root: &str, new: &str) -> Live {
    let d = format!("{root}{new}");
    if Path::new(&format!("{d}/{MARK}")).is_file() {
        Live::New
    } else if Path::new(&d).is_dir() {
        Live::HalfMoved
    } else {
        Live::Old
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree(String);
    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tree() -> (Tree, String) {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("u60d-paths-{}-{n}", std::process::id()));
        let r = d.to_str().unwrap().to_owned();
        std::fs::create_dir_all(&r).unwrap();
        (Tree(r.clone()), r)
    }

    fn mk(root: &str, dir: &str, mark: bool) {
        std::fs::create_dir_all(format!("{root}{dir}")).unwrap();
        if mark {
            std::fs::write(format!("{root}{dir}/{MARK}"), "").unwrap();
        }
    }

    #[test]
    fn core_names_by_prefix() {
        for n in ["zwrt-datad", "zwrt-datad.test", "luyaz-core", "luyaz-core.test"] {
            assert!(is_core(n), "{n}");
            assert_eq!(core_key(n), CORE);
        }
        for n in ["zte-agent", "u60-uid", "luyaz-sentinel", "zwrt-data", "luyaz-cor", ""] {
            assert!(!is_core(n), "{n}");
            assert_eq!(core_key(n), n);
        }
    }

    #[test]
    fn ui_names_by_prefix() {
        for n in ["luyaz-ui", "luyaz-ui.test"] {
            assert!(is_ui(n), "{n}");
        }
        for n in ["u60pro-devui", "u60pro-devui.te", "zte_topsw_devui", "luyaz-u", "luyaz-core", ""] {
            assert!(!is_ui(n), "{n}");
        }
        assert_eq!(prog_key("luyaz-ui"), UI);
        assert_eq!(prog_key("u60pro-devui"), UI);
        assert_eq!(prog_key("luyaz-ui.te"), "luyaz-ui.te", "side-by-side: as u60pro-devui.te was");
        assert_eq!(prog_key("luyaz-core"), CORE);
    }

    #[test]
    fn ship_cmdline_either_name() {
        assert!(is_ship_cmdline("sh\0/data/u60-guard/u60-ship.sh\0run"));
        assert!(is_ship_cmdline("sh /data/luyaz/scripts/luyaz-ship.sh run"));
        assert!(!is_ship_cmdline("sh /data/luyaz/scripts/doctor.sh"));
    }

    #[test]
    fn the_core_layout_follows_the_mark() {
        let (_d, r) = tree();
        assert_eq!(live(&r, DATA_NEW), Live::Old);
        mk(&r, DATA_NEW, false);
        assert_eq!(live(&r, DATA_NEW), Live::HalfMoved);
        mk(&r, DATA_NEW, true);
        assert_eq!(live(&r, DATA_NEW), Live::New);
    }
}
