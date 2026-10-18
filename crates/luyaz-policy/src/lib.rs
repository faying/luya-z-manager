//! U60 Pro (MU5250) policies (u60-platform.md Phase 4).
//!
//! Each policy is a pure function from typed input to typed output, plus the host-side state it
//! needs between calls. The pure part is what a wasm component exports (`wit/<policy>.wit` has the
//! same types); u60d calls the component when one is installed and these functions otherwise, so
//! both builds come from this one crate and are tested against the same cases.
//!
//! Moved from the retired zte-agent; the core is the single executor (10-08).

pub mod action;
pub mod apn_pick;
pub mod battery_eta;
pub mod charge_policy;
pub mod home_exit;
pub mod manual_first;
pub mod scenario;
pub mod scheduler;
pub mod sms_forward;
