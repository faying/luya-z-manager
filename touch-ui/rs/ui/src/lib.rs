//! The U60 Pro (MU5250) touch UI in Rust on LVGL 9.5 (manager docs/designs/u60-touch-rewrite.md).
//! M1–M4: theme, fonts, the components (kit), the chrome, the five tabs and every subpage, drawn
//! from a view model; M5: the data side (data) behind the screen's Source, and the device loop
//! (device). The offscreen harness (render) replays the C render test through the same pages.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

// kit functions take every position, size, colour and callback as its own argument
#![allow(clippy::too_many_arguments)]

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
