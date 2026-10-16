//! u60pro-devui: the touch UI (see lib.rs).
// SPDX-License-Identifier: MIT

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--render") {
        std::process::exit(devui::render::run(&args));
    }
    if std::env::var_os("LUYAZ_UI_EXEC_SELFTEST").is_some() {
        std::process::exit(devui::exec::selftest(&args));
    }
    std::process::exit(devui::device::run(&args));
}
