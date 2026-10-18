//! API pieces (moved from the retired zte-agent, u60-platform.md Phase 4 step 8) shared by the
//! core's touch compatibility layer and luyaz-web.

/// Moved to its own crate (u60-sentinel.md D9); re-exported so callers keep `luyaz_api::alerts`.
pub use luyaz_alerts as alerts;
pub mod auth;
pub mod charge;
pub mod chill;
pub mod clock;
pub mod control;
pub mod diagnose;
pub mod finding;
pub mod handover;
pub mod headers;
pub mod health;
pub mod lease;
pub mod netinfo;
pub mod scenario;
pub mod wifi;
