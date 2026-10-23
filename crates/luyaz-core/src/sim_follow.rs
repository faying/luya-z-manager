//! Who follows a card change (network reliability P1, 10-22): once a new SIM epoch has settled
//! back to `online` (`sim_identity::SimIdentity::settled`), CHILL re-renders and reloads (its
//! config is built against the bearer that just came back), and the home exit starts its miss
//! count over (misses on the old card say nothing about home).
//!
//! Once per epoch, on a thread of its own: the snapshot that noticed it is not held up.
//! CHILL only when it runs, through the one CHILL job slot; a job already running (start,
//! profile, reload) renders the current state itself, so a busy slot is skipped, not queued.

use std::sync::atomic::Ordering;

/// Woken whenever the SIM identity's epoch or phase moves (`phase`): the APN watch looks then
/// instead of waiting for its fallback poll.
pub fn moved() -> &'static tokio::sync::Notify {
    static N: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();
    N.get_or_init(tokio::sync::Notify::new)
}

/// Each snapshot round: wake [`moved`] when the epoch or phase differs from the last round.
pub fn phase(epoch: u64, phase: &'static str) {
    static LAST: std::sync::Mutex<(u64, &str)> = std::sync::Mutex::new((0, "online"));
    let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if *l != (epoch, phase) {
        *l = (epoch, phase);
        moved().notify_one();
    }
}

pub fn settled(epoch: u64) {
    eprintln!("[sim_follow] SIM epoch {epoch} online: telling CHILL and the home exit");
    crate::engines::home_exit::sim_changed();
    let spawned = std::thread::Builder::new()
        .name("sim-follow".into())
        .spawn(move || chill_reload(epoch));
    if let Err(e) = spawned {
        eprintln!("[sim_follow] could not start the CHILL reload: {e}");
    }
}

fn chill_running(state: Option<&str>) -> bool {
    state
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .is_some_and(|v| v["state"] == "running")
}

fn chill_reload(epoch: u64) {
    use luyaz_api::chill as ch;
    if !chill_running(std::fs::read_to_string(ch::CHILL_STATE).ok().as_deref()) {
        // off, or paused while idle: it renders afresh when it starts again
        eprintln!("[sim_follow] epoch {epoch}: CHILL not running, no reload");
        return;
    }
    let slot = crate::ops::chill::slot();
    if crate::ops::chill::sh_running() || slot.swap(true, Ordering::SeqCst) {
        eprintln!("[sim_follow] epoch {epoch}: a CHILL job is running, no extra reload");
        return;
    }
    let r = crate::engines::home_exit::chill_reload_within(
        &format!("{} reload", ch::CHILL_SH),
        crate::ops::chill::RELOAD_TIMEOUT,
    );
    slot.store(false, Ordering::SeqCst);
    match r {
        Ok(()) => eprintln!("[sim_follow] epoch {epoch}: CHILL reloaded"),
        Err(e) => eprintln!("[sim_follow] epoch {epoch}: CHILL {e}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_a_running_chill_is_reloaded() {
        use super::chill_running as r;
        assert!(r(Some(r#"{"state":"running","on":true}"#)));
        assert!(!r(Some(r#"{"state":"stopped"}"#)));
        assert!(!r(Some("not json")));
        assert!(!r(None));
    }
}
