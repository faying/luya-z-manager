//! `luyaz-sentinel`          run (procd; from 5f on, the screen owner)
//! `luyaz-sentinel --shadow` decide next to u60-uid and u60-guard and change nothing (§9 step 1);
//!                           everything to /tmp/luyaz-sentinel.shadow
//! `luyaz-sentinel --version`
//!
//! A real (non-shadow) run first takes the one-sentinel lock (`sys::INSTANCE_LOCK`, env
//! `LUYAZ_SENTINEL_LOCK` for tests); held by another copy = exit 3 before anything is touched.

/// Read from the file by the core (luyaz-core versions.rs), never by running the program.
#[used]
static VERSION_MARK: &str = concat!("LUYAZ_VERSION=", env!("CARGO_PKG_VERSION"), ";");

/// `<name> <version> (<commit>)`: the commit from LUYAZ_COMMIT (tools/luyaz ship passes it)
#[cfg(target_os = "linux")]
fn version_line(name: &str) -> String {
    match option_env!("LUYAZ_COMMIT").filter(|c| !c.is_empty()) {
        Some(c) => format!(
            "{name} {} ({})",
            env!("CARGO_PKG_VERSION"),
            &c[..c.len().min(7)]
        ),
        None => format!("{name} {}", env!("CARGO_PKG_VERSION")),
    }
}

#[cfg(target_os = "linux")]
fn main() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicI64;

    let arg = std::env::args().nth(1).unwrap_or_default();
    let shadow = match arg.as_str() {
        "" => false,
        "--shadow" => true,
        "--version" => {
            println!("{}", version_line("luyaz-sentinel"));
            return;
        }
        _ => {
            eprintln!("usage: luyaz-sentinel [--shadow | --version]");
            std::process::exit(2);
        }
    };
    // One sentinel at a time (any program name): a second copy leaves before touching anything.
    let _instance = if shadow {
        None
    } else {
        let lock = std::env::var("LUYAZ_SENTINEL_LOCK")
            .unwrap_or_else(|_| luyaz_sentinel::sys::INSTANCE_LOCK.into());
        match luyaz_sentinel::sys::single_instance(&lock) {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("luyaz-sentinel: not starting: {e}");
                std::process::exit(3);
            }
        }
    };
    // A panic anywhere ends the whole process (procd restarts it): a dead worker thread must never
    // sit behind a screen thread that keeps writing a healthy heartbeat.
    std::panic::set_hook(Box::new(|info| {
        eprintln!("luyaz-sentinel: panic: {info}");
        std::process::abort();
    }));
    let sig = luyaz_sentinel::run::install_signals();
    if !shadow {
        luyaz_sentinel::run::state_dir_at_start();
        luyaz_sentinel::run::start_events();
    }
    let round = Arc::new(AtomicI64::new(0));
    let r2 = round.clone();
    std::thread::Builder::new()
        .name("worker".into())
        .spawn(move || luyaz_sentinel::run::worker_thread(shadow, r2))
        .expect("worker thread");
    luyaz_sentinel::run::screen_thread(shadow, round, sig);
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("luyaz-sentinel runs on Linux only");
    std::process::exit(1);
}
