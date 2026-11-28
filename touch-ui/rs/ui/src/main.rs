//! luyaz-ui: the touch UI (see lib.rs).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

/// A log line that cannot be written is dropped instead of panicking: std's `eprintln!` panics when
/// the write fails, and the device build aborts on any panic, so a full /tmp (the UI's output is
/// /tmp/luyaz-ui.log) would crash the UI at its next line and on every relaunch until the sentinel
/// gave the panel to the vendor UI. Same as luyaz-core's `main.rs` (P-7); defined before every
/// `mod`, so the whole crate uses it.
#[allow(unused_macros)]
macro_rules! eprintln {
    ($($t:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stderr(), $($t)*);
    }};
}

/// [`eprintln!`] without the newline.
#[allow(unused_macros)]
macro_rules! eprint {
    ($($t:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::write!(::std::io::stderr(), $($t)*);
    }};
}

/// Read from the file by the core (luyaz-core versions.rs), never by running the program.
#[used]
static VERSION_MARK: &str = concat!("LUYAZ_VERSION=", env!("CARGO_PKG_VERSION"), ";");

/// mimalloc, as luyaz-core: musl's mallocng kept the per-event snapshot buffers' pages, RSS rose
/// ~13 KB per core event (3-8 MB/h on the device) until the next theme exec; glibc stayed flat.
#[global_allocator]
static HEAP: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// `<name> <version> (<commit>)`: the commit from LUYAZ_COMMIT (tools/luyaz ship passes it)
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--version") {
        println!("{}", version_line("luyaz-ui"));
        return;
    }
    if args.iter().any(|a| a == "--hitcheck") {
        std::process::exit(luyaz_ui::render::hitcheck(&args));
    }
    if args.iter().any(|a| a == "--brightcheck") {
        std::process::exit(luyaz_ui::render::brightcheck(&args));
    }
    if args.iter().any(|a| a == "--render") {
        std::process::exit(luyaz_ui::render::run(&args));
    }
    if std::env::var_os("LUYAZ_UI_EXEC_SELFTEST").is_some() {
        std::process::exit(luyaz_ui::exec::selftest(&args));
    }
    std::process::exit(luyaz_ui::device::run(&args));
}
