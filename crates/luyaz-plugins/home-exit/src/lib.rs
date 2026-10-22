//! The home-exit component: `luyaz_policy::home_exit::replay` behind `wit/home-exit.wit`. Only type
//! conversion here; the policy and its tests live in u60-policy.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/home-exit.wit", world: "home-exit" });

use luyaz_policy::home_exit as p;

fn items_in(i: Items) -> p::Items {
    p::Items { media: i.media, ip: i.ip, rest: i.rest }
}

fn items_out(i: p::Items) -> Items {
    Items { media: i.media, ip: i.ip, rest: i.rest }
}

fn fail_in(f: Fail) -> p::Fail {
    match f {
        Fail::Core => p::Fail::Core,
        Fail::Home => p::Fail::Home,
        Fail::Tailscale => p::Fail::Tailscale,
        Fail::Direct => p::Fail::Direct,
    }
}

fn fail_out(f: p::Fail) -> Fail {
    match f {
        p::Fail::Core => Fail::Core,
        p::Fail::Home => Fail::Home,
        p::Fail::Tailscale => Fail::Tailscale,
        p::Fail::Direct => Fail::Direct,
    }
}

fn machine_in(m: Machine) -> p::Machine {
    let q = m.p;
    let n = q.notice;
    p::Machine {
        p: p::Persist {
            items: items_in(q.items),
            rest_until: q.rest_until,
            reopened_in_china: items_in(q.reopened_in_china),
            notice: p::Notice { kind: n.kind, item: n.item, text: n.text, text_en: n.text_en, at: n.at, code: n.code, detail: n.detail },
        },
        last_probe: m.last_probe,
        fails: m.fails,
        idle: m.idle,
        home_ms: m.home_ms,
        home_ok: m.home_ok,
        probed_at: m.probed_at,
        china_since: m.china_since,
    }
}

fn machine_out(m: p::Machine) -> Machine {
    let q = m.p;
    let n = q.notice;
    Machine {
        p: Persist {
            items: items_out(q.items),
            rest_until: q.rest_until,
            reopened_in_china: items_out(q.reopened_in_china),
            notice: Notice { kind: n.kind, item: n.item, text: n.text, text_en: n.text_en, at: n.at, code: n.code, detail: n.detail },
        },
        last_probe: m.last_probe,
        fails: m.fails,
        idle: m.idle,
        home_ms: m.home_ms,
        home_ok: m.home_ok,
        probed_at: m.probed_at,
        china_since: m.china_since,
    }
}

fn event_in(e: Event) -> p::Event {
    match e {
        Event::ProbeHome(a) => p::Event::ProbeHome { answered: a.answered, ms: a.ms },
        Event::Apply(c) => p::Event::Apply { items: items_in(c.items), err: c.err },
        Event::Check(f) => p::Event::Check { fail: f.map(fail_in) },
        Event::Precheck(f) => p::Event::Precheck { fail: f.map(fail_in) },
        Event::Alert(kind) => p::Event::Alert { kind },
        Event::Save => p::Event::Save,
        Event::CellUp(up) => p::Event::CellUp { up },
    }
}

fn event_out(e: p::Event) -> Event {
    match e {
        p::Event::ProbeHome { answered, ms } => Event::ProbeHome(ProbeAnswer { answered, ms }),
        p::Event::Apply { items, err } => Event::Apply(ApplyCall { items: items_out(items), err }),
        p::Event::Check { fail } => Event::Check(fail.map(fail_out)),
        p::Event::Precheck { fail } => Event::Precheck(fail.map(fail_out)),
        p::Event::Alert { kind } => Event::Alert(kind),
        p::Event::Save => Event::Save,
        p::Event::CellUp { up } => Event::CellUp(up),
    }
}

struct Policy;

impl Guest for Policy {
    fn replay(before: Machine, s: Step, recorded: Vec<Event>) -> Outcome {
        let step = match s {
            Step::Tick(o) => p::Step::Tick {
                obs: p::Obs { now: o.now, serving_mcc: o.serving_mcc, env: items_in(o.env), clients: o.clients, screen_on: o.screen_on, cell_busy: o.cell_busy },
            },
            Step::Startup(s) => p::Step::Startup { had_file: s.had_file, env: items_in(s.env), now: s.now },
        };
        let out = p::replay(&machine_in(before), &step, recorded.into_iter().map(event_in).collect());
        Outcome {
            after: machine_out(out.after),
            calls: out.calls.into_iter().map(event_out).collect(),
            diverged: out.diverged.map(|(i, s)| (i as u32, s)),
            unused: out.unused as u32,
        }
    }
}

export!(Policy);
