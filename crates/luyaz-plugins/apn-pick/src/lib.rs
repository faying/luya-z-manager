//! The APN pick component: `luyaz_policy::apn_pick::replay` behind `wit/apn-pick.wit`. Only type
//! conversion here; the policy and its tests live in u60-policy.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/apn-pick.wit", world: "apn-pick" });

use luyaz_policy::apn_pick as p;

struct Policy;

impl Guest for Policy {
    fn decide(s: Store, before: Watch, iccid: String, busy: bool, apn: ApnRead) -> Look {
        let store = p::Store {
            picks: s.picks.into_iter().map(|(k, v)| (k, p::Pick { manual_id: v.manual_id, apn: v.apn })).collect(),
            last_iccid: s.last_iccid,
        };
        let before = p::Watch { pending: before.pending, seen: before.seen, fails: before.fails };
        let apn = match apn {
            ApnRead::NotAsked => p::ApnRead::NotAsked,
            ApnRead::Unread => p::ApnRead::Unread,
            ApnRead::Read(a) => p::ApnRead::Read { manual: a.manual, selected: a.selected },
        };
        let (d, w, asked) = p::replay(&store, &before, &iccid, busy, &apn);
        Look {
            action: match d.action {
                p::Action::None => Action::Keep,
                p::Action::Auto => Action::Auto,
                p::Action::Use(id) => Action::UseManual(id),
            },
            card: d.card,
            after: Watch { pending: w.pending, seen: w.seen, fails: w.fails },
            asked,
        }
    }
}

export!(Policy);
