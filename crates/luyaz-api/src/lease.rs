//! One owner of a loop across the agent and u60d: the owner writes
//! a lease (`<pid> <who>`) while its loop runs, and a loop starting up waits for another live
//! owner's lease to go before it reads its state, so the two never run at once. No file (or a dead
//! pid, or one that cannot be read) = free. Moved from u60-sms-forward (9e) for manual-first (9g).

use std::fs;
use std::path::Path;
use std::time::Duration;

/// How long a starting loop waits for another owner's lease.
pub const WAIT: Duration = Duration::from_secs(60);

/// Take the lease as this process (`who` names it in the file and the logs).
pub fn write(path: &Path, who: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, format!("{} {who}\n", std::process::id()))?;
    fs::rename(&tmp, path)
}

/// Give the lease back (only if it is ours).
pub fn remove(path: &Path) {
    if holder(path).is_some_and(|(pid, _)| pid == std::process::id()) {
        let _ = fs::remove_file(path);
    }
}

/// The lease's pid and owner, if the file is there and readable.
fn holder(path: &Path) -> Option<(u32, String)> {
    let text = fs::read_to_string(path).ok()?;
    let mut it = text.split_whitespace();
    let pid = it.next()?.parse().ok()?;
    Some((pid, it.next().unwrap_or("").to_string()))
}

/// The lease is held by another live process.
fn held_by_other(path: &Path) -> Option<(u32, String)> {
    holder(path).filter(|(pid, _)| {
        *pid != std::process::id() && Path::new(&format!("/proc/{pid}")).exists()
    })
}

/// Wait (1 s steps through `sleep`) while another live process holds the lease, at most `max`.
/// True if it is free (no file, our own, or its pid is gone). `tag` prefixes the log lines.
pub fn wait_free(path: &Path, tag: &str, sleep: &dyn Fn(Duration), max: Duration) -> bool {
    let Some((pid, who)) = held_by_other(path) else {
        return true;
    };
    eprintln!(
        "[{tag}] {who} (pid {pid}) holds {}: waiting up to {}s",
        path.display(),
        max.as_secs()
    );
    let mut waited = Duration::ZERO;
    while waited < max {
        sleep(Duration::from_secs(1));
        waited += Duration::from_secs(1);
        if held_by_other(path).is_none() {
            eprintln!("[{tag}] lease free after {}s", waited.as_secs());
            return true;
        }
    }
    eprintln!(
        "[{tag}] {who} (pid {pid}) still holds {} after {}s: going on",
        path.display(),
        max.as_secs()
    );
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn waits_only_for_another_live_owner() {
        let dir = std::env::temp_dir().join(format!("u60-lease-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let f = dir.join("x.owner");
        let slept = Cell::new(0u32);
        let sleep = |_d: Duration| slept.set(slept.get() + 1);
        // no file, our own, a dead pid, garbage: free at once
        assert!(wait_free(&f, "t", &sleep, Duration::from_secs(5)));
        write(&f, "me").unwrap();
        assert!(wait_free(&f, "t", &sleep, Duration::from_secs(5)));
        fs::write(&f, "999999999 gone\n").unwrap();
        assert!(wait_free(&f, "t", &sleep, Duration::from_secs(5)));
        fs::write(&f, "garbage").unwrap();
        assert!(wait_free(&f, "t", &sleep, Duration::from_secs(5)));
        assert_eq!(slept.get(), 0);
        // pid 1 is alive and not us: waits the whole time, then goes on
        fs::write(&f, "1 init\n").unwrap();
        assert!(!wait_free(&f, "t", &sleep, Duration::from_secs(3)));
        assert_eq!(slept.get(), 3);
        // remove() leaves another's lease alone, takes ours away
        remove(&f);
        assert!(f.exists());
        write(&f, "me").unwrap();
        remove(&f);
        assert!(!f.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
