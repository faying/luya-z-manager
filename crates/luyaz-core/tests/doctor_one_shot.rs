//! Phase 10 X11: `<core> doctor` runs before any of the core's side effects and under its own
//! process name, whatever argv[0] says (u60-phase10.md「X11 核心接管体检」).

#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn tree(d: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(d) {
        for e in rd.flatten() {
            v.push(e.path());
            v.extend(tree(&e.path()));
        }
    }
    v.sort();
    v
}

fn run(name: &str, copy: bool) {
    let t = std::env::temp_dir().join(format!("u60d-doctor-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let root = t.join("root");
    let fx = t.join("fx");
    std::fs::create_dir_all(root.join("data/luyaz/core")).unwrap();
    std::fs::create_dir_all(&fx).unwrap();
    // the stub ubus says who called it
    let ubus = fx.join("ubus");
    std::fs::write(
        &ubus,
        format!(
            "#!/bin/sh\ncat /proc/$PPID/comm >{}\n",
            fx.join("caller").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&ubus, std::fs::Permissions::from_mode(0o755)).unwrap();
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_luyaz-core"));
    let mut c = if copy {
        // a real file with a core's name: the kernel's first comm is core-like
        let p = t.join(name);
        std::fs::copy(&exe, &p).unwrap();
        Command::new(p)
    } else {
        let mut c = Command::new(&exe);
        c.arg0(name);
        c
    };
    let before = tree(&root);
    let o = c
        .args(["doctor", "--tsv"])
        .env("LUYAZ_HOST_ROOT", &root)
        .env("LUYAZ_PID_FILE", root.join("pid"))
        .env("DOC_UBUS", &ubus)
        .env("DOC_UCI", fx.join("none"))
        .env("DOC_DATE", fx.join("none"))
        .env("DOC_DF", fx.join("none"))
        .env("DOC_UPTIME", fx.join("uptime"))
        .env("DOC_CLOCK_OK", fx.join("clock-ok"))
        .env("DOC_WALL_LAST", fx.join("wall.last"))
        .env("DOC_ALERTS", fx.join("alerts"))
        .env("DOC_CRASH", fx.join("crash"))
        .env("DOC_DATA", &fx)
        .env("DOC_INITD", fx.join("initd"))
        .env("DOC_ROOT", &root)
        .env("DOC_LUYAZ_TMP", fx.join("tmp-luyaz"))
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.starts_with("bad\tboot-sync\t"), "{name}: {out}");
    assert_eq!(o.status.code(), Some(1), "{name}: bad rows, exit 1");
    assert_eq!(tree(&root), before, "{name}: wrote under the root");
    assert!(!root.join("pid").exists() && !root.join("data/luyaz/core/core.running").exists());
    let comm = std::fs::read_to_string(fx.join("caller")).unwrap();
    assert_eq!(comm.trim_end(), "luyaz-doctor", "{name}");
    let _ = std::fs::remove_dir_all(&t);
}

#[test]
fn doctor_under_every_core_name_is_one_shot_and_renamed() {
    for name in ["luyaz-core", "u60d"] {
        run(name, false);
    }
    run("luyaz-core.test", true);
}

/// R10: a hung `ubus` (and the sleep it started) costs one command deadline, not the run.
#[test]
fn a_hung_command_costs_its_deadline_not_the_run() {
    let t = std::env::temp_dir().join(format!("u60d-doctor-{}-hung", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let root = t.join("root");
    let fx = t.join("fx");
    std::fs::create_dir_all(root.join("data/luyaz/core")).unwrap();
    std::fs::create_dir_all(&fx).unwrap();
    let ubus = fx.join("ubus");
    std::fs::write(&ubus, "#!/bin/sh\nsleep 600 &\nsleep 600\n").unwrap();
    std::fs::set_permissions(&ubus, std::fs::Permissions::from_mode(0o755)).unwrap();
    let t0 = std::time::Instant::now();
    let o = Command::new(env!("CARGO_BIN_EXE_luyaz-core"))
        .args(["doctor", "--tsv2"])
        .env("LUYAZ_HOST_ROOT", &root)
        .env("LUYAZ_PID_FILE", root.join("pid"))
        .env("DOC_CMD_LIMIT_MS", "300")
        .env("DOC_UBUS", &ubus)
        .env("DOC_UCI", fx.join("none"))
        .env("DOC_DATE", fx.join("none"))
        .env("DOC_DF", fx.join("none"))
        .env("DOC_WGET", fx.join("none"))
        .env("DOC_UPTIME", fx.join("uptime"))
        .env("DOC_CLOCK_OK", fx.join("clock-ok"))
        .env("DOC_WALL_LAST", fx.join("wall.last"))
        .env("DOC_ALERTS", fx.join("alerts"))
        .env("DOC_CRASH", fx.join("crash"))
        .env("DOC_DATA", &fx)
        .env("DOC_INITD", fx.join("initd"))
        .env("DOC_ROOT", &root)
        .env("DOC_LUYAZ_TMP", fx.join("tmp-luyaz"))
        .output()
        .unwrap();
    let took = t0.elapsed();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("timed out after 300 ms"), "{err}");
    let out = String::from_utf8_lossy(&o.stdout);
    let row = out
        .lines()
        .find(|l| l.contains("\ttimeout\t"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(
        row.starts_with("warn\ttimeout\t") && row.contains("ubus"),
        "{row}"
    );
    assert!(took < std::time::Duration::from_secs(30), "took {took:?}");
    let _ = std::fs::remove_dir_all(&t);
}
