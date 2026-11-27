//! The U60 Pro (MU5250) touch UI in Rust on LVGL 9.5 (manager docs/designs/u60-touch-rewrite.md).
//! M1–M4: theme, fonts, the components (kit), the chrome, the five tabs and every subpage, drawn
//! from a view model; M5: the data side (data) behind the screen's Source, and the device loop
//! (device). The offscreen harness (render) replays the C render test through the same pages.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

// kit functions take every position, size, colour and callback as its own argument
#![allow(clippy::too_many_arguments)]

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

pub mod cfmt;
pub mod clock;
pub mod conf;
pub mod data;
pub mod device;
pub mod diag_view;
pub mod exec;
pub mod json;
pub mod kit;
pub mod lang;
pub mod logic;
pub mod lv;
pub mod net_view;
pub mod op_view;
pub mod render;
pub mod source;
#[cfg(test)]
pub mod testc;
pub mod theme;
pub mod ui;
