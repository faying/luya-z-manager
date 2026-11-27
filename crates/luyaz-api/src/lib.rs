//! API pieces shared by the
//! core's touch compatibility layer and luyaz-web.

/// A log line that cannot be written is dropped instead of panicking (std's `eprintln!` panics,
/// and the core's device build aborts on any panic: a full /tmp stopped it on 10-26). Same as
/// luyaz-core's `main.rs` (P-7); defined before every `mod`, so the whole crate uses it.
#[allow(unused_macros)]
macro_rules! eprintln {
    ($($t:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stderr(), $($t)*);
    }};
}

/// [`eprintln!`] without the newline.
#[allow(unused_macros)]
macro_rules! eprint {
    ($($t:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::write!(::std::io::stderr(), $($t)*);
    }};
}

/// Moved to its own crate (u60-sentinel.md D9); re-exported so callers keep `luyaz_api::alerts`.
pub use luyaz_alerts as alerts;
pub mod auth;
pub mod charge;
pub mod chill;
pub mod clock;
pub mod control;
pub mod diagnose;
pub mod execution;
pub mod finding;
pub mod firmware;
pub mod handover;
pub mod headers;
pub mod health;
pub mod lease;
pub mod netinfo;
pub mod scenario;
pub mod wifi;
