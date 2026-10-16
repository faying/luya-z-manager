//! U60 Pro (MU5250) policies (u60-platform.md Phase 4).
//!
//! Each policy is a pure function from typed input to typed output, plus the host-side state it
//! needs between calls. The pure part is what a wasm component exports (`wit/<policy>.wit` has the
//! same types); u60d calls the component when one is installed and these functions otherwise, so
//! both builds come from this one crate and are tested against the same cases.
//!
//! Moved from zte-agent while the agent still runs: u60d only computes and records what a policy
//! would do (shadow); the agent keeps acting until it retires (10-08, single executor).

pub mod action;
pub mod apn_pick;
pub mod battery_eta;
pub mod charge_policy;
pub mod scenario;
pub mod scheduler;
pub mod sms_forward;
