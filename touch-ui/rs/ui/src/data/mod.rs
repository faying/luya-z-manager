//! The data side on the device (manager docs/designs/u60-touch-rewrite.md §3): the view model,
//! built from the core's endpoints and the device's files.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

pub mod core_api;
pub mod alerts;
pub mod battery;
pub mod versions;
pub mod chill;
pub mod core_link;
pub mod diagnose;
pub mod esim;
pub mod http;
pub mod landing;
pub mod live;
pub mod netinfo;
pub mod scenario;
pub mod screen_feed;
pub mod speedtest;
pub mod tailscale;
#[cfg(test)]
pub mod testsrv;
pub mod util;
