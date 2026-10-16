//! The SMS forward component: `luyaz_policy::sms_forward::{replay_pass, settle}` behind
//! `wit/sms-forward.wit`. Only type conversion here; the policy and its tests live in u60-policy.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/sms-forward.wit", world: "sms-forward" });

use luyaz_policy::sms_forward as p;

fn delivery_in(d: Delivery) -> p::Delivery {
    match d {
        Delivery::Delivered => p::Delivery::Delivered,
        Delivery::Skipped => p::Delivery::Skipped(String::new()),
        Delivery::Deferred => p::Delivery::Deferred(String::new()),
    }
}

fn delivery_out(d: &p::Delivery) -> Delivery {
    match d {
        p::Delivery::Delivered => Delivery::Delivered,
        p::Delivery::Skipped(_) => Delivery::Skipped,
        p::Delivery::Deferred(_) => Delivery::Deferred,
    }
}

fn held_in(h: Held) -> p::HeldSms {
    p::HeldSms { sms_id: h.sms_id, first_deferred_at: h.first_deferred_at, online_secs: h.online_secs, delivered_rules: h.delivered_rules }
}

fn held_out(h: p::HeldSms) -> Held {
    Held { sms_id: h.sms_id, first_deferred_at: h.first_deferred_at, online_secs: h.online_secs, delivered_rules: h.delivered_rules }
}

struct Policy;

impl Guest for Policy {
    fn replay_pass(input: PassIn) -> PassOut {
        let pages = input
            .pages
            .into_iter()
            .map(|r| p::RecordedPage {
                after: r.after,
                limit: r.limit,
                answer: match r.answer {
                    PageAnswer::Ok(pg) => p::PageAnswer::Ok(p::Page {
                        items: pg.items.into_iter().map(|i| p::Item { id: i.id, received: i.received, time: i.time }).collect(),
                        has_more: pg.has_more,
                    }),
                    PageAnswer::Err(e) => p::PageAnswer::Err(e),
                },
            })
            .collect();
        let out = p::replay_pass(&p::PassIn {
            watermark: input.watermark,
            last_time: input.last_time,
            max_id: input.max_id,
            pages,
            forwards: input.forwards.into_iter().map(|(id, d)| (id, delivery_in(d))).collect(),
        });
        PassOut {
            asked: out.asked,
            forwarded: out.forwarded,
            commits: out.commits,
            ended: match out.result {
                Ok(n) => PassResult::Ok(n as u64),
                Err(e) => PassResult::Err(e),
            },
        }
    }

    fn settle(held: Option<Held>, sms_id: u64, failures: Vec<bool>, delivered: Vec<u32>, wall: i64) -> Settled {
        let (o, h, alert) = p::settle(held.map(held_in), sms_id, &failures, &delivered, wall);
        Settled { outcome: delivery_out(&o), held: h.map(held_out), alert }
    }
}

export!(Policy);
