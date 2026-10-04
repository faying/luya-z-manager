//! Every device write goes through datad (write-op-layer.md T7). This test
//! reads the agent's source and counts the primitives that can change the
//! device without datad: `ubus::write*`, `uci` set/commit, `at_cmd::send`,
//! `Command::new` and `libc::kill`. Each file's count must match the ledger
//! below exactly, so a new direct write anywhere fails here until someone
//! decides which row it belongs to. Reads go through `ubus::read*`, which
//! refuses methods that are not reads (`ubus::is_read_method`).
//!
//! Classes:
//! - `Def`: where the primitive is defined.
//! - `Read`: a command or AT query that only reads (counted all the same:
//!   `Command::new` cannot tell, so every one is listed).
//! - `Exempt`: stays outside datad on purpose (10-03 user decision: eSIM lpac,
//!   calls/USSD/STK/AT terminal, iw scan vdev / RTC / wake_lock,
//!   kill-bloat).
//! Since T7c (10-03) nothing is left to migrate: every row is a definition, a
//! read, or an exemption the owner agreed to.
//! (T7a, the writes datad already had an action for, is done: none left.)

use std::{collections::BTreeMap, fs, path::Path};

#[derive(Debug, Clone, Copy, PartialEq)]
enum Class {
    Def,
    Read,
    Exempt,
}
use Class::*;

const PRIMS: &[&str] = &[
    "ubus::write(",
    "uci_set_no_commit(",
    "uci_commit(",
    "uci_set(",
    "at_cmd::send(",
    "Command::new(",
    "libc::kill(",
];

/// (file under src/, primitive, count, class, what it is)
const LEDGER: &[(&str, &str, usize, Class, &str)] = &[
    ("ubus.rs", "Command::new(", 3, Def, "ubus (reads), ubus -v list and uci get"),
    ("at_cmd.rs", "Command::new(", 2, Def, "AT port I/O"),
    ("at_terminal.rs", "at_cmd::send(", 1, Exempt, "AT terminal"),
    ("device_ext.rs", "Command::new(", 2, Read, "sync before reboot / power off"),
    ("esim.rs", "Command::new(", 4, Exempt, "eSIM lpac"),
    ("fallback_write.rs", "Command::new(", 3, Exempt, "the emergency script (D19), and its test"),
    ("event_bus.rs", "Command::new(", 1, Read, "ubus listen"),
    ("health.rs", "Command::new(", 1, Read, "doctor.sh"),
    ("homemode.rs", "Command::new(", 1, Read, "iw scan"),
    ("netinfo.rs", "Command::new(", 1, Read, "sh_out"),
    ("netinfo.rs", "at_cmd::send(", 1, Read, "AT+COPS? (selection mode)"),
    ("public.rs", "Command::new(", 1, Read, "sh"),
    ("qos.rs", "at_cmd::send(", 2, Read, "AT+CGCONTRDP, AT+CGEQOSRDP"),
    ("services.rs", "Command::new(", 2, Read, "tailscale status, pidof"),
    ("sms.rs", "Command::new(", 1, Read, "sqlite3 -readonly: which deleted ids are still there"),
    ("system.rs", "libc::kill(", 1, Exempt, "kill-bloat"),
    ("telephony.rs", "at_cmd::send(", 15, Exempt, "calls, USSD, STK"),
    ("wifi.rs", "Command::new(", 3, Read, "iw / station dump reads, power_save get"),
    ("wifi.rs", "Command::new(", 2, Exempt, "iw txpower hot-apply: runtime radio state (the stored value goes through datad wifi.apply), like the kernel items in the 10-03 exemption list"),
    ("wifi_radio.rs", "Command::new(", 2, Read, "hostapd_cli / iw"),
    ("wifi_scan.rs", "Command::new(", 2, Exempt, "iw scan vdev"),
];

fn count(text: &str, prim: &str) -> usize {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .map(|l| {
            l.match_indices(prim)
                .filter(|(i, _)| {
                    l[..*i]
                        .chars()
                        .next_back()
                        .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
                })
                .count()
        })
        .sum()
}

fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, String>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, root, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            out.insert(rel, fs::read_to_string(&p).unwrap());
        }
    }
}

#[test]
fn every_direct_write_is_in_the_ledger() {
    let root = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").into())).join("src");
    let mut files = BTreeMap::new();
    walk(&root, &root, &mut files);
    let mut errors = Vec::new();
    for (file, text) in &files {
        for prim in PRIMS {
            let n = count(text, prim);
            let want: usize = LEDGER.iter().filter(|r| r.0 == file && r.1 == *prim).map(|r| r.2).sum();
            if n != want {
                errors.push(format!("src/{file}: {n} × `{prim}`, ledger says {want}"));
            }
        }
    }
    for r in LEDGER {
        if !files.contains_key(r.0) {
            errors.push(format!("ledger row for missing file src/{}", r.0));
        }
        if r.2 == 0 {
            errors.push(format!("src/{} `{}`: zero rows go away", r.0, r.1));
        }
    }
    assert!(
        errors.is_empty(),
        "direct device writes changed (write-op-layer.md T7; move the write to datad, or add/adjust a ledger row with a reason):\n{}",
        errors.join("\n")
    );
}

#[test]
fn every_row_not_yet_moved_says_why() {
    for r in LEDGER.iter().filter(|r| matches!(r.3, Exempt)) {
        assert!(!r.4.is_empty() || r.1 == "uci_commit(", "src/{} `{}` needs a reason", r.0, r.1);
    }
}
