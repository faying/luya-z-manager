//! The charge-policy component: `luyaz_policy::charge_policy::{enforce, on_unplug}` behind
//! `wit/charge-policy.wit`. Only type conversion here; the policy and its tests live in u60-policy.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/charge-policy.wit", world: "charge-policy" });

use luyaz_policy::charge_policy as p;

struct Policy;

impl Guest for Policy {
    fn enforce(b: Battery) -> Option<bool> {
        p::enforce(&p::Battery {
            capacity: b.capacity,
            discharging: b.discharging,
            stopped: b.stopped,
            limit: b.limit,
            hysteresis: b.hysteresis,
        })
    }

    fn on_unplug(stopped: bool) -> Option<bool> {
        p::on_unplug(stopped)
    }
}

export!(Policy);
