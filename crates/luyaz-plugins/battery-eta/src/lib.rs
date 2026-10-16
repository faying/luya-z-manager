//! The battery-eta component: `luyaz_policy::battery_eta::estimate` behind `wit/battery-eta.wit`.
//! Only type conversion here; the policy and its tests live in u60-policy.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/battery-eta.wit", world: "battery-eta" });

use luyaz_policy::battery_eta as p;

struct Policy;

impl Guest for Policy {
    fn estimate(input: Input) -> Estimate {
        let samples: Vec<p::Sample> = input
            .samples
            .iter()
            .map(|s| p::Sample {
                t: s.t,
                ua: s.ua,
                online: s.online,
            })
            .collect();
        let e = p::estimate(&p::Input {
            samples: &samples,
            soc: input.soc,
            charge_full_uah: input.charge_full_uah,
            charge_counter_uah: input.charge_counter_uah,
            target_pct: input.target_pct,
            paused_at_limit: input.paused_at_limit,
        });
        let kind = match e.kind {
            p::Kind::ChargingEta => Kind::ChargingEta,
            p::Kind::DischargingEta => Kind::DischargingEta,
            p::Kind::ReachedTarget => Kind::ReachedTarget,
            p::Kind::PausedAtLimit => Kind::PausedAtLimit,
            p::Kind::Unknown => Kind::Unknown,
        };
        Estimate {
            kind,
            minutes: e.minutes,
        }
    }
}

export!(Policy);
