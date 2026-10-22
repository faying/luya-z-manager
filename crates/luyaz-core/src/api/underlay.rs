//! Underlay gate (docs/research/network-reliability-underlay-20261021.md §6, P1).
//!
//! One question every repair asks before it acts: is the layer under mine healthy? It reads the
//! data interface's bearer (a default route out of it) and the open "sure" finding (with when it
//! was first seen), and answers from those. The only thing it keeps is who it last held, for
//! `network.health` ([`paused`]); that never feeds back into a verdict.
//!
//! - Structural repairs (a process died, a rule went missing) always pass: they fix what is
//!   known broken, whatever the cell does.
//! - Inferential repairs (a probe failed, so maybe my layer is broken) hold while a lower layer
//!   is down: the failure is the lower layer's, and acting on it only adds churn.
//! - No address on the data interface is hard evidence: hold for as long as it lasts (P0: the
//!   cell down 30 min must not close home-exit's rest).
//! - A lower finding is an inference: hold at most [`MAX_HOLD_SECS`], then let the caller act
//!   again, so a misread lower layer cannot freeze the upper one for good.
//!
//! Who asks: home-exit before counting a missed probe, manual-first before counting a timeout
//! (both through their host's `cell_up` / `cell_alive`). chill.sh's repairs are all structural
//! (rules lost, crash restart, captive WAN to direct, flush to direct), so it does not ask.

/// The layer that wants to act. Only the proxy layer asks yet: the IP layer's repair
/// (`data.reconnect`) is proposal-only and no_internet already offers it for IP/data only.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    Ip,
    Dns,
    /// mihomo, Tailscale, home-exit
    Proxy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Structural,
    Inferential,
}

/// What the gate reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Below {
    /// a default route out of `rmnet_data0`; None = could not read
    pub bearer: Option<bool>,
    /// the open sure finding: its layer ("signal", "data", "internet", "dns") and first seen
    pub finding: Option<(String, u64)>,
    /// unix seconds, the finding store's clock
    pub now: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    NoBearer,
    Lower { layer: String, secs: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Hold(Reason),
    /// held past [`MAX_HOLD_SECS`]: act as if allowed (logged)
    Released(Reason),
}

impl Verdict {
    pub fn holds(&self) -> bool {
        matches!(self, Verdict::Hold(_))
    }
}

/// The longest an inferred lower outage holds an upper repair.
pub const MAX_HOLD_SECS: u64 = 15 * 60;

/// 0 = signal … 3 = dns; unknown layers rank as none (they never hold).
fn rank_of_finding(layer: &str) -> Option<u8> {
    match layer {
        "signal" => Some(0),
        "data" => Some(1),
        "internet" => Some(2),
        "dns" => Some(3),
        _ => None,
    }
}

fn rank(l: Layer) -> u8 {
    match l {
        Layer::Ip => 2,
        Layer::Dns => 3,
        Layer::Proxy => 4,
    }
}

pub fn gate(layer: Layer, kind: Kind, b: &Below) -> Verdict {
    if kind == Kind::Structural {
        return Verdict::Allow;
    }
    if b.bearer == Some(false) {
        return Verdict::Hold(Reason::NoBearer);
    }
    let Some((f, first)) = &b.finding else {
        return Verdict::Allow;
    };
    match rank_of_finding(f) {
        Some(r) if r < rank(layer) => {
            let secs = b.now.saturating_sub(*first);
            let why = Reason::Lower { layer: f.clone(), secs };
            if secs < MAX_HOLD_SECS {
                Verdict::Hold(why)
            } else {
                Verdict::Released(why)
            }
        }
        _ => Verdict::Allow,
    }
}

/// The live inputs (`/proc` reads only).
pub fn below() -> Below {
    Below {
        bearer: crate::ops::reconnect::proc_bearer(""),
        finding: super::finding::home_now().map(|h| (h.layer, h.first)),
        now: luyaz_netwatch::now_unix(),
    }
}

/// Who the gate held last time it asked, and when the last hold cleared.
#[derive(Default)]
struct Held {
    /// who → (the hold, when last asked)
    by: std::collections::BTreeMap<String, (luyaz_api::execution::network_health::Paused, u64)>,
    cleared: Option<u64>,
}

static HELD: std::sync::Mutex<Held> = std::sync::Mutex::new(Held {
    by: std::collections::BTreeMap::new(),
    cleared: None,
});

fn held() -> std::sync::MutexGuard<'static, Held> {
    HELD.lock().unwrap_or_else(|e| e.into_inner())
}

/// Note `who`'s verdict: a hold keeps its first time, a release marks it, an allow clears it.
fn note(who: &str, v: &Verdict, now: u64) {
    use luyaz_api::execution::network_health::Paused;
    let mut h = held();
    let (r, released) = match v {
        Verdict::Allow => {
            if h.by.remove(who).is_some() {
                h.cleared = Some(now);
            }
            return;
        }
        Verdict::Hold(r) => (r, false),
        Verdict::Released(r) => (r, true),
    };
    let (reason, layer) = match r {
        Reason::NoBearer => ("no_bearer", None),
        Reason::Lower { layer, .. } => ("lower", Some(layer.clone())),
    };
    let (e, last) = h.by.entry(who.to_string()).or_insert((Paused { who: who.to_string(), reason, layer: None, since: now, released }, now));
    (e.reason, e.layer, e.released, *last) = (reason, layer, released, now);
}

/// Drop the holds whose cause is gone (the askers only ask after a miss, so nobody says "allow"
/// once things work again): no bearer once it is back, a lower finding once none is open.
fn settle(bearer: Option<bool>, finding: Option<&str>, now: u64) {
    let mut h = held();
    let before = h.by.len();
    h.by.retain(|_, (p, _)| match p.reason {
        "no_bearer" => bearer != Some(true),
        _ => finding.is_some_and(|f| rank_of_finding(f).is_some()),
    });
    if h.by.len() < before {
        h.cleared = Some(now);
    }
}

/// A hold nobody asked about again for this long is dropped (its asker stopped: CHILL off…).
const STALE_SECS: u64 = 30 * 60;

/// The repairs held now, and seconds since the last one cleared (for `network.health`).
pub fn paused(now: u64) -> (Vec<luyaz_api::execution::network_health::Paused>, Option<u64>) {
    let h = held();
    let live = h.by.values().filter(|(_, last)| now.saturating_sub(*last) < STALE_SECS);
    (live.map(|(p, _)| p.clone()).collect(), h.cleared.map(|t| now.saturating_sub(t)))
}

/// `network.health`'s inputs: the bearer under `root`, the open sure finding, the held repairs.
pub fn health_input(root: &str) -> luyaz_api::execution::network_health::Input {
    use luyaz_api::execution::network_health as nh;
    let now = luyaz_netwatch::now_unix();
    let bearer = crate::ops::reconnect::proc_bearer(root);
    let home = super::finding::home_now();
    settle(bearer, home.as_ref().map(|h| h.layer.as_str()), now);
    let (paused, recovered_secs) = paused(now);
    nh::Input {
        bearer,
        finding: home.map(|h| nh::Finding {
            kind: h.kind,
            layer: h.layer,
            symptom: h.symptom,
            symptom_en: h.symptom_en,
        }),
        paused,
        recovered_secs,
        proxy: crate::engines::chill_probe::now(),
    }
}

/// [`gate`] on the live inputs, logged when it does not simply allow.
pub fn ask(who: &str, layer: Layer, kind: Kind) -> Verdict {
    let b = below();
    let v = gate(layer, kind, &b);
    note(who, &v, b.now);
    match &v {
        Verdict::Allow => {}
        Verdict::Hold(r) => eprintln!("[underlay] {who}: hold ({r:?})"),
        Verdict::Released(r) => eprintln!("[underlay] {who}: held past {MAX_HOLD_SECS} s, acting ({r:?})"),
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_800_000_000;

    fn below(bearer: Option<bool>, finding: Option<(&str, u64)>) -> Below {
        Below {
            bearer,
            finding: finding.map(|(l, s)| (l.to_string(), T - s)),
            now: T,
        }
    }

    #[test]
    fn structural_repairs_pass_with_the_cell_down() {
        let b = below(Some(false), Some(("signal", 3600)));
        for l in [Layer::Ip, Layer::Dns, Layer::Proxy] {
            assert_eq!(gate(l, Kind::Structural, &b), Verdict::Allow);
        }
    }

    #[test]
    fn healthy_underlay_allows() {
        assert_eq!(gate(Layer::Proxy, Kind::Inferential, &below(Some(true), None)), Verdict::Allow);
    }

    #[test]
    fn no_address_holds_without_a_limit() {
        // P0: the cell down 40 min still holds home-exit, nothing is released
        let b = below(Some(false), Some(("data", 40 * 60)));
        assert_eq!(gate(Layer::Proxy, Kind::Inferential, &b), Verdict::Hold(Reason::NoBearer));
        assert!(gate(Layer::Proxy, Kind::Inferential, &below(Some(false), None)).holds());
    }

    #[test]
    fn unreadable_address_does_not_hold() {
        assert_eq!(gate(Layer::Proxy, Kind::Inferential, &below(None, None)), Verdict::Allow);
    }

    #[test]
    fn a_lower_finding_holds_the_proxy() {
        for l in ["signal", "data", "internet", "dns"] {
            let v = gate(Layer::Proxy, Kind::Inferential, &below(Some(true), Some((l, 60))));
            assert_eq!(v, Verdict::Hold(Reason::Lower { layer: l.into(), secs: 60 }), "{l}");
        }
    }

    #[test]
    fn dns_broken_does_not_hold_ip() {
        // DNS down with bearer and IP fine: only DNS is marked, IP repairs are not held
        let b = below(Some(true), Some(("dns", 60)));
        assert_eq!(gate(Layer::Ip, Kind::Inferential, &b), Verdict::Allow);
        assert_eq!(gate(Layer::Dns, Kind::Inferential, &b), Verdict::Allow);
        assert!(gate(Layer::Proxy, Kind::Inferential, &b).holds());
    }

    #[test]
    fn same_layer_finding_does_not_hold_itself() {
        let b = below(Some(true), Some(("internet", 60)));
        assert_eq!(gate(Layer::Ip, Kind::Inferential, &b), Verdict::Allow);
    }

    #[test]
    fn an_inferred_hold_ends_at_fifteen_minutes() {
        let v = gate(Layer::Proxy, Kind::Inferential, &below(Some(true), Some(("internet", MAX_HOLD_SECS - 1))));
        assert!(v.holds());
        let v = gate(Layer::Proxy, Kind::Inferential, &below(Some(true), Some(("internet", MAX_HOLD_SECS))));
        assert_eq!(v, Verdict::Released(Reason::Lower { layer: "internet".into(), secs: MAX_HOLD_SECS }));
        assert!(!v.holds());
    }

    #[test]
    fn unknown_finding_layers_never_hold() {
        let b = below(Some(true), Some(("exit", 60)));
        assert_eq!(gate(Layer::Proxy, Kind::Inferential, &b), Verdict::Allow);
    }

    #[test]
    fn holds_are_noted_and_cleared_without_changing_the_verdict() {
        let b = below(Some(false), None);
        let v = gate(Layer::Proxy, Kind::Inferential, &b);
        note("t-held", &v, T);
        assert_eq!(gate(Layer::Proxy, Kind::Inferential, &b), v);
        let mine = |now| paused(now).0.into_iter().find(|p| p.who == "t-held");
        let p = mine(T).unwrap();
        assert_eq!((p.reason, p.since, p.released), ("no_bearer", T, false));
        // still held later: the first time stays; released: marked
        note("t-held", &Verdict::Released(Reason::Lower { layer: "data".into(), secs: 900 }), T + 900);
        let p = mine(T + 900).unwrap();
        assert_eq!((p.reason, p.layer.as_deref(), p.since, p.released), ("lower", Some("data"), T, true));
        note("t-held", &Verdict::Allow, T + 1000);
        assert!(mine(T + 1000).is_none());
        assert_eq!(paused(T + 1010).1, Some(10));
        // recovered with nobody asking again: the cause is gone, so is the hold
        note("t-bearer", &Verdict::Hold(Reason::NoBearer), T);
        note("t-lower", &Verdict::Hold(Reason::Lower { layer: "data".into(), secs: 0 }), T);
        settle(Some(false), Some("data"), T + 5);
        let mine = |w: &str| paused(T + 5).0.iter().any(|p| p.who == w);
        assert!(mine("t-bearer") && mine("t-lower"));
        settle(Some(true), Some("data"), T + 6);
        assert!(!mine("t-bearer") && mine("t-lower"));
        settle(Some(true), None, T + 7);
        assert!(!mine("t-lower"));
        // an asker that stopped asking: its hold goes stale
        note("t-stale", &Verdict::Hold(Reason::NoBearer), T);
        assert!(paused(T + STALE_SECS - 1).0.iter().any(|p| p.who == "t-stale"));
        assert!(!paused(T + STALE_SECS).0.iter().any(|p| p.who == "t-stale"));
    }

    #[test]
    fn a_clock_behind_the_finding_holds() {
        let b = Below { bearer: Some(true), finding: Some(("data".into(), T + 100)), now: T };
        assert_eq!(gate(Layer::Proxy, Kind::Inferential, &b), Verdict::Hold(Reason::Lower { layer: "data".into(), secs: 0 }));
    }
}
