//! The engines: what runs by itself in the background (watchers, the guard's duties, the CHILL
//! idle rules, the AI gate, manual-first, the scenario engine, the scheduler, SMS forwarding, the home exit),
//! apart from the API routes in [`crate::api`] that read and steer them (Phase 10 §4). They share
//! the API's state, [`crate::api::Compat`], and are started by [`crate::api::Compat::resume`].

pub(crate) mod chill_ai_gate;
pub(crate) mod chill_idle;
pub(crate) mod chill_probe;
pub(crate) mod guard_duties;
pub(crate) mod home_exit;
pub(crate) mod manual_first;
pub(crate) mod scenario;
pub(crate) mod scheduler;
pub(crate) mod sentinel_watch;
pub(crate) mod sms_forward;
pub(crate) mod wifi_dfs;
