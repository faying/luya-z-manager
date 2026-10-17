//! zte-agent's API pieces shared with u60d's touch compatibility layer (u60-platform.md Phase 4
//! step 8). The agent keeps serving the admin web until Phase 8; u60d serves the touch screen.

/// Moved to its own crate (u60-sentinel.md D9); re-exported so callers keep `luyaz_api::alerts`.
pub use luyaz_alerts as alerts;
pub mod auth;
pub mod charge;
pub mod chill;
pub mod clock;
pub mod control;
pub mod diagnose;
pub mod handover;
pub mod headers;
pub mod health;
pub mod lease;
pub mod netinfo;
pub mod scenario;
pub mod wifi;
