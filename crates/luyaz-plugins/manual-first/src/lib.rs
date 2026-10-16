//! The manual-first component: `luyaz_policy::manual_first::{replay, Machine::needs_detail}` behind
//! `wit/manual-first.wit`. Only type conversion here; the policy and its tests live in u60-policy.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/manual-first.wit", world: "manual-first" });

use luyaz_policy::manual_first as p;

fn probe_in(x: Probe) -> p::Probe {
    match x {
        Probe::Ms(ms) => p::Probe::Ms(ms),
        Probe::Timeout => p::Probe::Timeout,
    }
}

fn probe_out(x: p::Probe) -> Probe {
    match x {
        p::Probe::Ms(ms) => Probe::Ms(ms),
        p::Probe::Timeout => Probe::Timeout,
    }
}

fn machine_in(m: Machine) -> p::Machine {
    let q = m.p;
    p::Machine {
        p: p::Persist {
            backup: q.backup,
            manual: q.manual,
            switched_at: q.switched_at,
            switches: q.switches,
            last_region: q.last_region,
            notice: q.notice,
            notice_en: q.notice_en,
            notice_at: q.notice_at,
            notice_kind: q.notice_kind,
        },
        manual: m.manual,
        timeouts: m.timeouts,
        bad: m.bad,
        good_since: m.good_since,
        last_probe: m.last_probe,
    }
}

fn machine_out(m: p::Machine) -> Machine {
    let q = m.p;
    Machine {
        p: Persist {
            backup: q.backup,
            manual: q.manual,
            switched_at: q.switched_at,
            switches: q.switches,
            last_region: q.last_region,
            notice: q.notice,
            notice_en: q.notice_en,
            notice_at: q.notice_at,
            notice_kind: q.notice_kind,
        },
        manual: m.manual,
        timeouts: m.timeouts,
        bad: m.bad,
        good_since: m.good_since,
        last_probe: m.last_probe,
    }
}

fn event_in(e: Event) -> p::Event {
    match e {
        Event::InUse(got) => p::Event::InUse { got },
        Event::Probe(c) => p::Event::Probe { node: c.node, got: c.got.map(probe_in) },
        Event::CellAlive(got) => p::Event::CellAlive { got },
        Event::DirectMs(got) => p::Event::DirectMs { got },
        Event::RegionOf(c) => p::Event::RegionOf { node: c.node, got: c.got },
        Event::Retest(c) => p::Event::Retest { group: c.group, got: c.got },
        Event::SelectMain(c) => p::Event::SelectMain { member: c.member, ok: c.ok },
        Event::DropManualConns => p::Event::DropManualConns,
        Event::Save => p::Event::Save,
    }
}

fn event_out(e: p::Event) -> Event {
    match e {
        p::Event::InUse { got } => Event::InUse(got),
        p::Event::Probe { node, got } => Event::Probe(ProbeCall { node, got: got.map(probe_out) }),
        p::Event::CellAlive { got } => Event::CellAlive(got),
        p::Event::DirectMs { got } => Event::DirectMs(got),
        p::Event::RegionOf { node, got } => Event::RegionOf(RegionCall { node, got }),
        p::Event::Retest { group, got } => Event::Retest(RetestCall { group, got }),
        p::Event::SelectMain { member, ok } => Event::SelectMain(SelectCall { member, ok }),
        p::Event::DropManualConns => Event::DropManualConns,
        p::Event::Save => Event::Save,
    }
}

struct Policy;

impl Guest for Policy {
    fn replay(before: Machine, o: Obs, recorded: Vec<Event>) -> Outcome {
        let obs = p::Obs { now: o.now, active: o.active, main_now: o.main_now, manual_now: o.manual_now };
        let out = p::replay(&machine_in(before), &obs, recorded.into_iter().map(event_in).collect());
        Outcome {
            after: machine_out(out.after),
            calls: out.calls.into_iter().map(event_out).collect(),
            diverged: out.diverged.map(|(i, s)| (i as u32, s)),
            unused: out.unused as u32,
        }
    }

    fn needs_detail(m: Machine, main_now: String) -> bool {
        machine_in(m).needs_detail(&main_now)
    }
}

export!(Policy);
