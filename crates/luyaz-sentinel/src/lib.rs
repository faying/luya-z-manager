//! luyaz-sentinel — the Luya-Z sentinel on the U60 Pro (MU5250), u60-sentinel.md Phase 5.
//!
//! Two jobs, sized by one question — "must this still happen when the core (u60d) is dead?":
//!  * the screen owner ([`screen`]): the only thing that starts or stops our touch UI and the only
//!    thing that hands the panel to the vendor UI;
//!  * [`worker`]: Wi-Fi fallback, modem crash recovery, the clock verdict, log caps, ship
//!    recover-live, dropbear, and the core's health.
//!
//! Decisions are pure ([`screen::Screen::step`]) or written against a device trait
//! ([`worker::WorkerIo`], [`wifi::WifiOps`]), so the tests run on a fake device (D10); `sys` is the
//! real one, `run` the two threads and shadow mode.

#[cfg(target_os = "linux")]
pub mod run;
pub mod screen;
#[cfg(target_os = "linux")]
pub mod sys;
pub mod events;
pub mod frame;
mod frame_text;
pub mod wifi;
pub mod worker;

#[cfg(test)]
mod tests;
