//! Headers a loopback caller sets on u60d's `/v2/api/...` (believed only from loopback). The
//! retired agent set them on the calls it forwarded (Phase 4 step 9); luyaz-web, the scheduler and
//! the scenario engine set them now.

/// The address of the client that asked luyaz-web (deep diagnosis picks the Wi-Fi rule by it).
pub const CLIENT_HEADER: &str = "X-U60-Client";

/// Who a call is for, as the write source (`web`, `scheduler`, `scenario`), so u60d's write
/// carries it. Without it a loopback request is the screen's.
pub const SOURCE_HEADER: &str = "X-U60-Source";

/// On a call the scenario engine makes while putting things back (its restores): a service switched on
/// by a restore is not a switch by hand, so u60d keeps the restore entry (the restore drops it once
/// it worked).
pub const RESTORE_HEADER: &str = "X-U60-Restore";
