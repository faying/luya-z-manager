//! luyaz-ui: the touch UI (see lib.rs).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

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
    if args.iter().any(|a| a == "--render") {
        std::process::exit(luyaz_ui::render::run(&args));
    }
    if std::env::var_os("LUYAZ_UI_EXEC_SELFTEST").is_some() {
        std::process::exit(luyaz_ui::exec::selftest(&args));
    }
    std::process::exit(luyaz_ui::device::run(&args));
}
