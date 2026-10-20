//! The decision loop's proposal producer (decision-execution-loop.md §4.9).
//!
//! After each finding check: an open `data.no_internet` finding for the current card → one
//! [`Evidence`] → [`replay`]. Only when the advice names an action (a lost data session or
//! address → `data.reconnect`) is a [`Proposal`] made, and only with every binding known:
//! the finding's store epoch + revision, the full card, the SIM/connection/radio epochs from one
//! collector scope, this boot. Nothing here runs anything: a proposal waits for an approval
//! ([`super::execute::Loop::execute`]), and [`super::execute::ALLOW`] is empty.
//!
//! Where the evidence comes from:
//! - premises and registration: the finding check's own inputs (same round);
//! - data session and address: the rmnet addresses (`ops::reconnect::session`), not
//!   `get_wwaniface`, which keeps saying connected while the session is down (B31, 10-20);
//! - the radio epoch: O5 as the owner closed it, a real ONLINE mode read just before
//!   ([`fresh_mode_read`]); without it the epoch is unknown and nothing is proposed.
//!
//! Pending proposals live in memory only: a core restart voids them (never replayed, §1.1).

use std::sync::Mutex;

use luyaz_api::clock::BootTime;
use luyaz_api::execution::no_internet::{Advice, Evidence, replay};
use luyaz_api::execution::{
    Epochs, FindingRef, FindingSnapshot, Now, Proposal, RollbackPolicy, SimId,
};
use luyaz_api::finding::{KIND_NO_INTERNET, Premises, Reg, expected_outage};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// A proposal is good for this long.
pub const TTL_MS: u64 = 5 * 60 * 1000;
/// The device has one SIM slot.
pub const SLOT: u8 = 1;
/// At proposal time the mode read is the producer's own, just done.
pub const RADIO_PROPOSE_S: u64 = 5;
/// Under the engine lock: the approver's fresh read plus the Gate wait (T4 measured 4.8 s) and
/// the old-value read must fit. Not [`RADIO_PROPOSE_S`]: the gate wait alone would refuse
/// almost every attempt as `RadioUnknown`.
pub const RADIO_EXEC_S: u64 = 30;

/// The one canonical digest of an action's parameters (the approval side uses it too).
/// `serde_json`'s maps are sorted (no `preserve_order`), so equal params give equal text.
pub fn params_digest(action: &str, params: &Value) -> String {
    let mut h = Sha256::new();
    h.update(action.as_bytes());
    h.update(b"\n");
    h.update(params.to_string().as_bytes());
    hex(&h.finalize()[..16])
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// This boot (cached: the precheck runs under the engine lock and must not open files).
pub fn boot_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(crate::evlog::boot_id)
}

/// The evidence for `replay`. `rmnet_up`: the data interface has a global address (None = the
/// read failed); it stands for both the session and the address.
pub fn evidence(
    premises: &Premises,
    reg: Reg,
    same_sim: Option<bool>,
    passive_down_s: Option<u64>,
    age_s: Option<u64>,
    rmnet_up: Option<bool>,
) -> Evidence {
    Evidence {
        passive_down_s,
        expected_outage: expected_outage(premises).ok().map(|e| e.is_some()),
        same_sim,
        registered: match reg {
            Reg::Home | Reg::Roaming => Some(true),
            Reg::Searching | Reg::Denied | Reg::NotRegistered => Some(false),
            Reg::Unknown => None,
        },
        data_session: rmnet_up,
        has_ip: rmnet_up,
        age_s,
        // the later layers don't decide the one action; left unknown
        ..Evidence::default()
    }
}

/// What a proposal binds to, gathered by the caller.
pub struct Binding<'a> {
    pub finding_epoch: &'a str,
    pub finding_revision: u64,
    pub iccid: &'a str,
    pub epochs: Epochs,
    pub boot_id: &'a str,
    pub boot_ms: Option<u64>,
    /// The engine's user-write count now: part of the id, so a human write makes a new
    /// proposal and an approval of the old one no longer matches.
    pub user_writes: u64,
}

/// A proposal for `advice`, or None: no action, or any binding unknown.
pub fn build(advice: &Advice, b: &Binding) -> Option<Proposal> {
    let action = advice.action()?;
    let known = |s: &Option<String>| s.as_deref().is_some_and(|s| !s.is_empty());
    if b.finding_epoch.is_empty()
        || b.iccid.is_empty()
        || b.boot_id.is_empty()
        || !(known(&b.epochs.sim) && known(&b.epochs.conn) && known(&b.epochs.radio))
    {
        return None;
    }
    let at = b.boot_ms?;
    let mut h = Sha256::new();
    for part in [
        b.boot_id,
        b.finding_epoch,
        &b.finding_revision.to_string(),
        b.epochs.sim.as_deref().unwrap_or(""),
        b.epochs.conn.as_deref().unwrap_or(""),
        b.epochs.radio.as_deref().unwrap_or(""),
        &b.user_writes.to_string(),
        action,
    ] {
        h.update(part.as_bytes());
        h.update(b"\n");
    }
    Some(Proposal {
        proposal_id: hex(&h.finalize()[..12]),
        finding: FindingRef {
            finding_id: KIND_NO_INTERNET.into(),
            kind: KIND_NO_INTERNET.into(),
            epoch: b.finding_epoch.into(),
            revision: b.finding_revision,
        },
        boot_id: b.boot_id.into(),
        sim: SimId {
            iccid: b.iccid.into(),
            slot: SLOT,
        },
        epochs: b.epochs.clone(),
        action_id: action.into(),
        params_digest: params_digest(action, &json!({})),
        reason: match advice {
            Advice::ReadOnly { layer, .. } => serde_json::to_value(layer)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            _ => String::new(),
        },
        excluded: vec!["expected_outage".into(), "sim_changed".into()],
        // the redial can't be undone (`ops/reconnect.rs`: Unsupported)
        rollback_policy: RollbackPolicy::None,
        created_at_boot_ms: at,
        expires_at_boot_ms: at + TTL_MS,
    })
}

/// A proposal waiting for an approval, with what the precheck compares against.
#[derive(Clone, Debug)]
pub struct Pending {
    pub proposal: Proposal,
    /// The engine's user-write count when proposed (`human_wrote` = it moved).
    pub user_writes: u64,
}

impl Pending {
    /// For `/api/diagnose`: the card as its tail only.
    pub fn view(&self) -> Value {
        let p = &self.proposal;
        let tail = &p.sim.iccid[p.sim.iccid.len().saturating_sub(4)..];
        json!({
            "proposal_id": p.proposal_id,
            "kind": p.finding.kind,
            "action": p.action_id,
            "params_digest": p.params_digest,
            "reason": p.reason,
            "rollback": p.rollback_policy,
            "iccid_tail": tail,
            "created_at_boot_ms": p.created_at_boot_ms,
            "expires_at_boot_ms": p.expires_at_boot_ms,
        })
    }
}

/// The current proposal (at most one: there is one finding kind and one action).
#[derive(Default)]
pub struct Producer {
    pending: Mutex<Option<Pending>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Producer {
    /// The pending proposal while it is unexpired on this boot.
    pub fn current(&self, boot_ms: Option<u64>) -> Option<Pending> {
        let p = lock(&self.pending).clone()?;
        let live = p.proposal.boot_id == boot_id()
            && boot_ms.is_some_and(|t| t < p.proposal.expires_at_boot_ms);
        live.then_some(p)
    }

    pub fn view(&self) -> Value {
        self.current(Some(crate::evlog::boottime_ms()))
            .map(|p| p.view())
            .unwrap_or(Value::Null)
    }

    pub fn clear(&self) {
        *lock(&self.pending) = None;
    }

    /// Keep `p` unless the same binding is already pending (same id): no new id, no new read.
    pub fn offer(&self, p: Pending) {
        let mut g = lock(&self.pending);
        if g.as_ref()
            .is_some_and(|o| o.proposal.proposal_id == p.proposal.proposal_id)
        {
            return;
        }
        *g = Some(p);
    }

    /// Already pending for this finding revision, connection and user-write count: skip the
    /// mode read.
    pub fn has(&self, epoch: &str, revision: u64, conn: Option<&str>, user_writes: u64) -> bool {
        lock(&self.pending).as_ref().is_some_and(|o| {
            let p = &o.proposal;
            p.finding.epoch == epoch
                && p.finding.revision == revision
                && p.epochs.conn.as_deref() == conn
                && o.user_writes == user_writes
        })
    }
}

/// `true` when `rmnet_data0` has a global address; None when either read failed (`ip` not
/// exiting 0, e.g. no such device during a modem restart, or `if_inet6` unreadable other than
/// missing, which means no IPv6). Unknown never becomes "down". Blocking.
pub fn rmnet_up() -> Option<bool> {
    let out = std::process::Command::new("ip")
        .args([
            "-4",
            "-o",
            "addr",
            "show",
            "dev",
            crate::ops::reconnect::IFACE,
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    rmnet_state(
        out.as_deref(),
        std::fs::read_to_string("/proc/net/if_inet6"),
    )
}

/// [`rmnet_up`] on its two reads.
pub fn rmnet_state(v4: Option<&str>, if_inet6: std::io::Result<String>) -> Option<bool> {
    let v6 = match if_inet6 {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return None,
    };
    Some(crate::ops::reconnect::session(v4?, &v6) == "up")
}

/// One real (uncached) read of the radio mode into the collector (O5). Blocking.
pub fn fresh_mode_read(hub: &crate::block::Hub) {
    let fw = hub.firmware();
    let token = fw.token();
    let out = std::process::Command::new("ubus")
        .args([
            "-t",
            "3",
            "call",
            "uci",
            "get",
            r#"{"config":"zte_nwinfo","section":"sys_info","option":"operate_mode"}"#,
        ])
        .output();
    let value = out
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .and_then(|v| v["value"].as_str().map(str::to_owned));
    fw.radio_read(token, value.as_deref(), BootTime::read());
}

/// After one finding check (blocking; `finding_round`): keep, replace or drop the proposal.
/// `readings` = the round's own, `hub` = for the current card and the epochs.
pub fn after_round(
    prod: &Producer,
    findings: &super::finding::Findings,
    i: &super::finding::Inputs,
    readings: Option<&super::finding::sampling::Readings>,
    hub: Option<&crate::block::Hub>,
) {
    use luyaz_netwatch::finding::FindingHost;
    let (Some(hub), Some(r)) = (hub, readings) else {
        return prod.clear();
    };
    let iccid = r.connection.iccid.as_str();
    let Some(id) = findings.identity(KIND_NO_INTERNET, iccid) else {
        return prod.clear();
    };
    let boot = BootTime::read();
    let age = boot.as_ref().and_then(|b| b.elapsed_since(&r.observed));
    let down = findings.down_secs(Some(r), boot.as_ref());
    let same_sim =
        super::finding::sampling::Readings::capture(hub).map(|c| c.connection.iccid == iccid);
    let e = evidence(
        &i.premises(),
        i.registration(),
        same_sim,
        down,
        age,
        rmnet_up(),
    );
    let advice = replay(&e);
    if advice.action().is_none() {
        return prod.clear();
    }
    let Some(user_writes) = crate::ops::engine::device_user_writes() else {
        return prod.clear();
    };
    let conn = hub
        .firmware()
        .loop_epochs(boot.as_ref(), RADIO_PROPOSE_S)
        .0
        .conn;
    if prod.has(&id.epoch, id.revision, conn.as_deref(), user_writes) {
        return;
    }
    // only now, once per finding revision, connection and user write: battery
    fresh_mode_read(hub);
    let (epochs, _) = hub
        .firmware()
        .loop_epochs(BootTime::read().as_ref(), RADIO_PROPOSE_S);
    let b = Binding {
        finding_epoch: &id.epoch,
        finding_revision: id.revision,
        iccid,
        epochs,
        boot_id: boot_id(),
        boot_ms: Some(crate::evlog::boottime_ms()).filter(|&t| t > 0),
        user_writes,
    };
    match build(&advice, &b) {
        // used already (refused, or ended without a new connection): the same binding never
        // comes back as a button that can only answer "already used"
        Some(proposal) if spent(&proposal.proposal_id) => prod.clear(),
        Some(proposal) => prod.offer(Pending {
            proposal,
            user_writes,
        }),
        None => prod.clear(),
    }
}

/// The device loop's spent ids (tests: never spent, there is no loop file).
fn spent(id: &str) -> bool {
    !cfg!(test) && super::approve::device_loop().is_spent(id)
}

/// The precheck's re-read (`Loop::execute`'s `current`): memory only, so safe under the engine
/// lock. The user-write count is the engine's atomic, read here under the lock (never through
/// the engine, whose lock is held): a person's write just before still voids the approval.
pub fn now_for(
    hub: &crate::block::Hub,
    findings: &super::finding::Findings,
    pending: &Pending,
) -> (FindingSnapshot, Now) {
    let p = &pending.proposal;
    let boot = BootTime::read();
    let (epochs, device_id) = hub.firmware().loop_epochs(boot.as_ref(), RADIO_EXEC_S);
    let iccid = super::finding::sampling::Readings::capture(hub).map(|r| r.connection.iccid);
    let snap = findings.snapshot(&p.finding.kind, &p.sim.iccid);
    let user_writes = crate::ops::engine::device_user_writes();
    (
        snap,
        Now {
            boot_id: boot_id().into(),
            boot_ms: Some(crate::evlog::boottime_ms()).filter(|&t| t > 0),
            device_id,
            sim: iccid.map(|iccid| SimId { iccid, slot: SLOT }),
            epochs,
            gate_granted: true,
            // unknown counts as written: refuse
            human_wrote: user_writes != Some(pending.user_writes),
        },
    )
}

/// The device's producer, finding store and event log, for the approval (`/control` `loop.approve`, which
/// runs where the engine is). Set once by the core's API at start; tests build their own.
pub struct Shared {
    pub producer: std::sync::Arc<Producer>,
    pub findings: std::sync::Arc<super::finding::Findings>,
    /// where a finding the loop resolves sends its `finding.close`
    pub watch: std::sync::Arc<luyaz_netwatch::Watch>,
}

static SHARED: std::sync::OnceLock<Shared> = std::sync::OnceLock::new();

pub fn register(s: Shared) {
    let _ = SHARED.set(s);
}

pub fn shared() -> Option<&'static Shared> {
    SHARED.get()
}

#[cfg(test)]
#[path = "tests_propose.rs"]
mod tests;
