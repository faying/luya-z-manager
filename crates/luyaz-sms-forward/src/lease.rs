//! Who forwards: the owner writes a lease (`<pid> <who>`) while it runs, and a forwarder starting
//! up waits for another live owner's lease to go before its first pass, so two never send the
//! same SMS. No file (or a dead pid) = free. The mechanism is u60-api's [`luyaz_api::lease`].

use std::path::Path;
use std::time::Duration;

use crate::Host;

/// On /tmp: gone at boot, like the owner that wrote it.
pub const LEASE_FILE: &str = "/tmp/sms-forward.owner";
/// How long a starting forwarder waits for another owner's lease.
pub const LEASE_WAIT: Duration = luyaz_api::lease::WAIT;

/// Take the lease as this process (`who` names it in the file and the logs).
pub fn write_lease(path: &Path, who: &str) -> std::io::Result<()> {
    luyaz_api::lease::write(path, who)
}

/// Give the lease back (only if it is ours).
pub fn remove_lease(path: &Path) {
    luyaz_api::lease::remove(path)
}

/// Wait (1 s steps through `host`) while another live process holds the lease, at most `max`.
/// True if it is free (no file, our own, or its pid is gone).
pub fn wait_lease_free(path: &Path, host: &dyn Host, max: Duration) -> bool {
    luyaz_api::lease::wait_free(path, "sms_forward", &|d| host.sleep(d), max)
}
