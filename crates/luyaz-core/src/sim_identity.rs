//! SimIdentity (P0-2, 10-22): which card identity the modem stack is on, and where a change of
//! it stands, as one phase plus an epoch the UIs and rules can key on.
//!
//! Pure: no I/O, a monotonic clock passed in. Fed by the eSIM switch's steps (`intent`,
//! `job_ended`) and by every snapshot's `get_sim_info` + bearer read (`observe`); it has no
//! timer of its own, so an idle device costs nothing beyond the snapshot it already takes.
//!
//! Phases:
//! - `online`: identity known and the bearer up (or nothing has happened yet).
//! - `leaving`: a switch announced itself, or the identity went empty; the old one is going.
//! - `changed`: a new identity (ICCID+IMSI) is in; the bearer is not back yet.
//! - `reloaded`: the stack re-read and came back on the same identity (same card).
//! - `stalled`: `leaving`/`changed`/`reloaded` longer than `STALL` without the bearer.
//!
//! `epoch` goes up by one each time an identity transition starts (`leaving`) or a new identity
//! shows up without one; it never goes down in a run of the core.

use std::time::{Duration, Instant};

/// Past this in a transition without the bearer: `stalled`. The eSIM switch's own worst case
/// (identity 45 s + data 90 s, plus the stack restart) is inside it.
pub const STALL: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Online,
    Leaving,
    Changed,
    Reloaded,
    Stalled,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Online => "online",
            Phase::Leaving => "leaving",
            Phase::Changed => "changed",
            Phase::Reloaded => "reloaded",
            Phase::Stalled => "stalled",
        }
    }
}

/// ICCID (BCD pad nibble dropped) + IMSI. Empty IMSI = no identity.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Identity {
    pub iccid: String,
    pub imsi: String,
}

impl Identity {
    pub fn new(iccid: &str, imsi: &str) -> Self {
        Identity {
            iccid: iccid.trim().trim_end_matches(['F', 'f']).to_string(),
            imsi: imsi.trim().to_string(),
        }
    }
    fn present(&self) -> bool {
        !self.imsi.is_empty()
    }
    /// The same card: the IMSI decides; ICCIDs only when both are known
    /// (`sim_iccid` reads empty for a beat after a re-read).
    fn same(&self, o: &Identity) -> bool {
        self.imsi == o.imsi
            && (self.iccid.is_empty() || o.iccid.is_empty() || self.iccid == o.iccid)
    }
}

#[derive(Debug)]
pub struct SimIdentity {
    epoch: u64,
    phase: Phase,
    since: Option<Instant>,
    known: Option<Identity>,
    /// The identity a transition is leaving (to tell `changed` from `reloaded`).
    from: Option<Identity>,
    /// A switch job is between `intent` and `job_ended`.
    switching: bool,
    /// The last epoch [`SimIdentity::settled`] handed out.
    notified: u64,
}

impl Default for SimIdentity {
    fn default() -> Self {
        SimIdentity {
            epoch: 0,
            phase: Phase::Online,
            since: None,
            known: None,
            from: None,
            switching: false,
            notified: 0,
        }
    }
}

impl SimIdentity {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    /// The identity last seen (none before the first read or while there is no card).
    pub fn known(&self) -> Option<&Identity> {
        self.known.as_ref()
    }

    /// Once per epoch, when its transition has settled back to `online`: the epoch, for the
    /// listeners that must follow a card change (CHILL reload, home exit's miss count).
    /// The identity the core started on is epoch 0 and never reported.
    pub fn settled(&mut self) -> Option<u64> {
        (self.phase == Phase::Online && self.epoch > self.notified).then(|| {
            self.notified = self.epoch;
            self.epoch
        })
    }

    fn enter(&mut self, p: Phase, now: Instant) {
        if self.phase != p {
            self.phase = p;
            self.since = Some(now);
        }
    }

    fn start_transition(&mut self, now: Instant) {
        if !matches!(self.phase, Phase::Leaving) {
            self.epoch += 1;
            self.from = self.known.clone();
        }
        self.enter(Phase::Leaving, now);
    }

    /// An eSIM switch is starting (`luyaz_esim::SwitchEvent::Leaving`).
    pub fn intent(&mut self, now: Instant) {
        self.switching = true;
        self.start_transition(now);
    }

    /// The switch job ended, whichever way. A switch that never left the old identity
    /// (card refused, busy) shows up as `reloaded` and settles to `online` with the bearer.
    pub fn job_ended(&mut self, _now: Instant) {
        self.switching = false;
    }

    /// One read: the stack's identity (empty IMSI = none) and whether the bearer is up.
    pub fn observe(&mut self, id: Identity, bearer: bool, now: Instant) {
        if !id.present() {
            if self.known.is_some() || self.switching {
                self.start_transition(now);
            }
            self.stall_check(bearer, now);
            return;
        }
        let first = self.known.is_none() && self.from.is_none() && self.epoch == 0;
        let moved = match &self.known {
            Some(k) => !k.same(&id),
            None => false,
        };
        // keep the fuller spelling (ICCID fills in late)
        if self.known.as_ref().is_none_or(|k| !k.same(&id) || k.iccid.is_empty()) {
            self.known = Some(id.clone());
        }
        if first {
            // the identity the core started on: nothing changed
            self.since.get_or_insert(now);
            return;
        }
        if moved && !matches!(self.phase, Phase::Leaving | Phase::Changed | Phase::Stalled) {
            // a new identity with no announced transition (the firmware's own slot switch)
            self.epoch += 1;
        }
        match self.phase {
            Phase::Leaving => {
                let back_on_old = self.from.as_ref().is_some_and(|f| f.same(&id));
                if !back_on_old {
                    self.enter(Phase::Changed, now);
                } else if !self.switching {
                    self.enter(Phase::Reloaded, now);
                }
                // still switching and the old identity reads: the stack has not moved yet
            }
            Phase::Online if moved => self.enter(Phase::Changed, now),
            Phase::Reloaded if moved => self.enter(Phase::Changed, now),
            Phase::Stalled if moved => self.enter(Phase::Changed, now),
            _ => {}
        }
        if bearer && matches!(self.phase, Phase::Changed | Phase::Reloaded | Phase::Stalled) {
            self.from = None;
            self.enter(Phase::Online, now);
        }
        self.stall_check(bearer, now);
    }

    fn stall_check(&mut self, bearer: bool, now: Instant) {
        let in_transition = matches!(self.phase, Phase::Leaving | Phase::Changed | Phase::Reloaded);
        if in_transition
            && !bearer
            && self.since.is_some_and(|t| now.saturating_duration_since(t) >= STALL)
        {
            self.enter(Phase::Stalled, now);
        }
    }
}

/// The core's one instance (the snapshot reads it, the eSIM engine's threads write it).
pub fn global() -> &'static std::sync::Mutex<SimIdentity> {
    static S: std::sync::OnceLock<std::sync::Mutex<SimIdentity>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: (&str, &str) = ("89852000000000000001", "440100000000001");
    const B: (&str, &str) = ("89852000000000000002", "440100000000002");

    fn id(x: (&str, &str)) -> Identity {
        Identity::new(x.0, x.1)
    }
    fn none() -> Identity {
        Identity::default()
    }

    struct T {
        s: SimIdentity,
        t0: Instant,
    }
    impl T {
        fn new() -> Self {
            let mut t = T { s: SimIdentity::default(), t0: Instant::now() };
            t.s.observe(id(A), true, t.at(0));
            t
        }
        fn at(&self, s: u64) -> Instant {
            self.t0 + Duration::from_secs(s)
        }
        fn see(&mut self, x: Identity, bearer: bool, s: u64) -> (Phase, u64) {
            self.s.observe(x, bearer, self.at(s));
            (self.s.phase(), self.s.epoch())
        }
    }

    #[test]
    fn starts_online_at_epoch_zero() {
        let t = T::new();
        assert_eq!((t.s.phase(), t.s.epoch()), (Phase::Online, 0));
    }

    #[test]
    fn a_steady_identity_stays_online() {
        let mut t = T::new();
        for s in 1..100 {
            assert_eq!(t.see(id(A), true, s), (Phase::Online, 0));
        }
    }

    #[test]
    fn bearer_loss_alone_is_not_an_identity_change() {
        let mut t = T::new();
        assert_eq!(t.see(id(A), false, 5), (Phase::Online, 0));
        assert_eq!(t.see(id(A), false, 500), (Phase::Online, 0));
    }

    #[test]
    fn a_switch_goes_leaving_changed_online() {
        let mut t = T::new();
        t.s.intent(t.at(1));
        assert_eq!((t.s.phase(), t.s.epoch()), (Phase::Leaving, 1));
        assert_eq!(t.see(id(A), true, 2), (Phase::Leaving, 1), "stack not moved yet");
        assert_eq!(t.see(none(), false, 4), (Phase::Leaving, 1));
        assert_eq!(t.see(id(B), false, 7), (Phase::Changed, 1));
        t.s.job_ended(t.at(20));
        assert_eq!(t.see(id(B), true, 20), (Phase::Online, 1));
    }

    #[test]
    fn the_new_identity_with_the_bearer_already_up_goes_straight_online() {
        let mut t = T::new();
        t.s.intent(t.at(1));
        assert_eq!(t.see(id(B), true, 5), (Phase::Online, 1));
    }

    #[test]
    fn a_refused_switch_is_a_reload_of_the_same_card() {
        let mut t = T::new();
        t.s.intent(t.at(1));
        assert_eq!(t.see(id(A), false, 2), (Phase::Leaving, 1));
        t.s.job_ended(t.at(3));
        assert_eq!(t.see(id(A), false, 4), (Phase::Reloaded, 1));
        assert_eq!(t.see(id(A), true, 6), (Phase::Online, 1));
    }

    #[test]
    fn no_bearer_for_too_long_stalls_and_recovers() {
        let mut t = T::new();
        t.s.intent(t.at(0));
        assert_eq!(t.see(id(B), false, 10), (Phase::Changed, 1));
        assert_eq!(t.see(id(B), false, 10 + STALL.as_secs() - 1).0, Phase::Changed);
        assert_eq!(t.see(id(B), false, 10 + STALL.as_secs()).0, Phase::Stalled);
        assert_eq!(t.see(id(B), true, 400), (Phase::Online, 1));
    }

    #[test]
    fn leaving_with_no_identity_ever_back_stalls() {
        let mut t = T::new();
        t.s.intent(t.at(0));
        assert_eq!(t.see(none(), false, STALL.as_secs()).0, Phase::Stalled);
    }

    #[test]
    fn stalled_then_a_new_identity_is_changed_not_a_new_epoch() {
        let mut t = T::new();
        t.s.intent(t.at(0));
        t.see(none(), false, STALL.as_secs());
        assert_eq!(t.see(id(B), false, 200), (Phase::Changed, 1));
    }

    #[test]
    fn an_unannounced_identity_change_counts_an_epoch() {
        // the firmware's own slot switch (全球版 smart switch): no intent
        let mut t = T::new();
        assert_eq!(t.see(id(B), false, 10), (Phase::Changed, 1));
        assert_eq!(t.see(id(B), true, 15), (Phase::Online, 1));
    }

    #[test]
    fn the_identity_going_empty_is_leaving() {
        let mut t = T::new();
        assert_eq!(t.see(none(), false, 3), (Phase::Leaving, 1));
        // same card back (a modem re-read)
        assert_eq!(t.see(id(A), false, 6), (Phase::Reloaded, 1));
        assert_eq!(t.see(id(A), true, 8), (Phase::Online, 1));
    }

    #[test]
    fn repeated_empty_reads_do_not_bump_the_epoch() {
        let mut t = T::new();
        for s in 1..20 {
            assert_eq!(t.see(none(), false, s).1, 1);
        }
    }

    #[test]
    fn a_late_iccid_is_the_same_identity() {
        // `sim_iccid` reads empty for a beat after a re-read
        let mut t = T::new();
        t.s.intent(t.at(0));
        assert_eq!(t.see(Identity::new("", B.1), false, 3), (Phase::Changed, 1));
        assert_eq!(t.see(id(B), false, 4), (Phase::Changed, 1));
        assert_eq!(t.see(id(B), true, 5), (Phase::Online, 1));
    }

    #[test]
    fn the_bcd_pad_nibble_is_ignored() {
        assert!(Identity::new("8985200000000000072F", "1").same(&Identity::new("8985200000000000072", "1")));
    }

    #[test]
    fn two_switches_are_two_epochs() {
        let mut t = T::new();
        t.s.intent(t.at(0));
        t.see(id(B), true, 5);
        t.s.job_ended(t.at(6));
        t.s.intent(t.at(100));
        assert_eq!(t.see(id(A), true, 105), (Phase::Online, 2));
    }

    #[test]
    fn an_intent_while_already_leaving_is_one_transition() {
        let mut t = T::new();
        t.see(none(), false, 1);
        t.s.intent(t.at(2));
        assert_eq!((t.s.phase(), t.s.epoch()), (Phase::Leaving, 1));
    }

    #[test]
    fn a_switch_back_onto_the_old_card_after_a_change_is_changed() {
        let mut t = T::new();
        t.s.intent(t.at(0));
        t.see(id(B), false, 3);
        // the stack flips back to A before the bearer: a change again, not a reload
        t.s.job_ended(t.at(4));
        assert_eq!(t.see(id(A), false, 5).0, Phase::Changed);
        assert_eq!(t.see(id(A), true, 6).0, Phase::Online);
    }

    #[test]
    fn online_has_no_timer() {
        let mut t = T::new();
        assert_eq!(t.see(id(A), false, 100_000).0, Phase::Online);
    }

    #[test]
    fn settled_reports_each_epoch_once_when_back_online() {
        let mut t = T::new();
        assert_eq!(t.s.settled(), None, "the starting card is not a change");
        t.s.intent(t.at(0));
        t.see(id(B), false, 3);
        assert_eq!(t.s.settled(), None, "not online yet");
        t.see(id(B), true, 9);
        assert_eq!(t.s.settled(), Some(1));
        assert_eq!(t.s.settled(), None, "once");
        t.see(id(B), true, 40);
        assert_eq!(t.s.settled(), None);
        // a firmware slot switch with no intent: the next epoch
        t.see(id(A), false, 50);
        t.see(id(A), true, 55);
        assert_eq!(t.s.settled(), Some(2));
    }
}
