//! The data side on the device (manager docs/designs/u60-touch-rewrite.md §3: devui-data): the
//! C UI's data modules, function by function, producing the same view model from the same
//! endpoints and files.
// SPDX-License-Identifier: MIT

pub mod core_api;
pub mod alerts;
pub mod battery;
pub mod chill;
pub mod datad;
pub mod diagnose;
pub mod esim;
pub mod http;
pub mod live;
pub mod netinfo;
pub mod scenario;
pub mod screen_feed;
pub mod speedtest;
pub mod tailscale;
#[cfg(test)]
pub mod testsrv;
pub mod util;
