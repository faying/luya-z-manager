//! The engines: what runs by itself in the background (watchers, the guard's duties,
//! the scenario engine, the scheduler, SMS forwarding),
//! apart from the API routes in [`crate::api`] that read and steer them (Phase 10 §4). They share
//! the API's state, [`crate::api::Compat`], and are started by [`crate::api::Compat::resume`].

pub(crate) mod guard_duties;
pub(crate) mod scenario;
pub(crate) mod scheduler;
pub(crate) mod sentinel_watch;
pub(crate) mod sms_forward;
