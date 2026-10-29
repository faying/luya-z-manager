use super::*;
use std::sync::atomic::AtomicUsize;

fn snap(max_id: u64, received: &[(u64, Option<i64>)]) -> DeviceSnapshot {
    DeviceSnapshot { max_id, received: received.to_vec() }
}

/// 短信同类场景: core drops, an SMS arrives, the direct poll (within
/// 60 s) forwards it; core comes back, the source-switch pass finds
/// nothing new — no second send.
#[test]
fn sms_during_fallback_forwarded_once() {
    let s = |x: u64| Duration::from_secs(x);
    let mut sched = PollSchedule::new(s(0), true);
    let mut dev = FakeDev::new(1..=100);
    let mut st = Persisted::new(100, Some(1_000_000));
    let mut sent: Vec<u64> = Vec::new();
    // core drops at 100 → source switch, checks once (nothing new).
    assert!(sched.source_changed(false));
    st.pass(&dev, None, &mut sent).unwrap();
    sched.polled(s(100));
    // SMS 101 arrives at 110 (no core wake).
    dev.add(101..=101);
    let mut now = 110;
    while !sched.due(s(now)) {
        let w = sched.wait(s(now));
        assert!(w <= Duration::from_secs(60));
        now += w.as_secs().max(1);
    }
    assert!(now - 110 <= 60, "direct poll within 60 s, got {}", now - 110);
    st.pass(&dev, None, &mut sent).unwrap();
    sched.polled(s(now));
    assert_eq!(sent, vec![101]);
    // core back: source switch, pass via core, nothing new.
    assert!(sched.source_changed(true));
    assert!(!sched.source_changed(true), "same source: no extra wake");
    st.pass(&dev, None, &mut sent).unwrap();
    sched.polled(s(now + 30));
    assert_eq!(sent, vec![101], "no re-send after recovery");
    assert!(!sched.due(s(now + 30 + 299)), "via core: back to the 5-min net");
}

// ── T10 paging ──

/// A device: ids in two stores (multiples of 5 on SIM), one shared
/// counter; `zte_libwms_get_sms_data` pages descending only.
struct FakeDev {
    ids: Vec<u64>,
    sent_tags: Vec<u64>,
}

impl FakeDev {
    fn new(ids: impl IntoIterator<Item = u64>) -> Self {
        FakeDev { ids: ids.into_iter().collect(), sent_tags: Vec::new() }
    }
    fn add(&mut self, ids: impl IntoIterator<Item = u64>) {
        self.ids.extend(ids);
    }
    fn max_id(&self) -> u64 {
        self.ids.iter().copied().max().unwrap_or(0)
    }
    fn store_page(&self, args: &Value) -> Result<Value, String> {
        if args["order_by"] != "order by id desc" {
            return Err("Invalid argument".into());
        }
        let sim = args["mem_store"] == 0;
        let mut ids: Vec<u64> = self.ids.iter().copied().filter(|i| (i % 5 == 0) == sim).collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        let per = args["data_per_page"].as_u64().unwrap() as usize;
        let page = args["page"].as_u64().unwrap() as usize;
        let rows: Vec<Value> = ids
            .iter()
            .skip(page * per)
            .take(per)
            .map(|&id| {
                let tag = if self.sent_tags.contains(&id) { "2" } else { "1" };
                // 00:00 + id minutes, so later ids are later in time.
                let date = format!("26,08,27,{:02},{:02},00,+32", (id / 60) % 24, id % 60);
                json!({"id": id.to_string(), "number": "0031003000300038003600", "content": "0041", "tag": tag, "date": date})
            })
            .collect();
        Ok(json!({"messages": rows}))
    }
    /// the core's `sms.list_after` over this device (same algorithm).
    fn list_after(&self, after: u64, limit: u64, calls: &mut usize) -> Result<Page, String> {
        *calls += 1;
        list_after_direct(after, limit, &mut |store, page| {
            self.store_page(&json!({"tags": 10, "page": page, "data_per_page": LIST_PAGE, "mem_store": store, "order_by": "order by id desc"}))
        })
    }
}

/// The forwarder's state file, round-tripped through JSON on every
/// commit like `save_state` / `SmsForwarder::new`.
struct Persisted {
    json: String,
}

impl Persisted {
    fn new(wm: u64, lt: Option<i64>) -> Self {
        let st = ForwardState { last_forwarded_id: wm, last_forwarded_time: lt, log: Vec::new(), held: None };
        Persisted { json: serde_json::to_string(&st).unwrap() }
    }
    fn load(&self) -> (u64, Option<i64>) {
        let st: ForwardState = serde_json::from_str(&self.json).unwrap();
        (st.last_forwarded_id, st.last_forwarded_time)
    }
    /// One pass; `fail_on_call` makes that fetch call fail (interruption).
    fn pass(&mut self, dev: &FakeDev, fail_on_call: Option<usize>, sent: &mut Vec<u64>) -> Result<usize, String> {
        let (wm, lt) = self.load();
        let mut calls = 0;
        let mut fetch = |after: u64, limit: u64| {
            if fail_on_call == Some(calls + 1) {
                return Err("core went away".to_string());
            }
            dev.list_after(after, limit, &mut calls)
        };
        let mut forward = |sms: &DecodedSms| {
            sent.push(sms.id);
            Delivery::Delivered
        };
        let json = &mut self.json;
        let mut commit = |wm: u64, lt: Option<i64>| {
            let st = ForwardState { last_forwarded_id: wm, last_forwarded_time: lt, log: Vec::new(), held: None };
            *json = serde_json::to_string(&st).unwrap();
        };
        run_pass(wm, lt, dev.max_id(), &mut fetch, &mut forward, &mut commit)
    }
}

/// One pass where `outcome(id)` decides each delivery.
fn pass_with(st: &mut Persisted, dev: &FakeDev, sent: &mut Vec<u64>, outcome: &dyn Fn(u64) -> Delivery) -> usize {
    let (wm, lt) = st.load();
    let mut calls = 0;
    let json = &mut st.json;
    run_pass(
        wm,
        lt,
        dev.max_id(),
        &mut |a, l| dev.list_after(a, l, &mut calls),
        &mut |s: &DecodedSms| {
            let o = outcome(s.id);
            if !matches!(o, Delivery::Deferred(_)) {
                sent.push(s.id);
            }
            o
        },
        &mut |w, t| *json = serde_json::to_string(&ForwardState { last_forwarded_id: w, last_forwarded_time: t, log: Vec::new(), held: None }).unwrap(),
    )
    .unwrap()
}

/// No link: 3 SMS arrive, nothing moves; link back: all 3 in order, once.
#[test]
fn sms_offline_held_then_forwarded_in_order() {
    let mut dev = FakeDev::new(1..=10);
    let mut st = Persisted::new(10, Some(0));
    dev.add(11..=13);
    let mut sent = Vec::new();
    let offline = |_| delivery_outcome(&[NO_WAN_ERR.to_string()]);
    for _ in 0..3 {
        assert_eq!(pass_with(&mut st, &dev, &mut sent, &offline), 0);
        assert_eq!(st.load(), (10, Some(0)), "watermark and last time held");
    }
    assert!(sent.is_empty());
    assert_eq!(pass_with(&mut st, &dev, &mut sent, &|_| Delivery::Delivered), 3);
    once_each(&sent, 11..=13);
    assert_eq!(st.load().0, 13);
    assert_eq!(pass_with(&mut st, &dev, &mut sent, &|_| Delivery::Delivered), 0, "no repeat");
    once_each(&sent, 11..=13);
}

/// A permanent (4xx) failure skips that SMS and moves on.
#[test]
fn sms_permanent_error_skipped() {
    let mut dev = FakeDev::new(1..=10);
    let mut st = Persisted::new(10, Some(0));
    dev.add(11..=13);
    let mut sent = Vec::new();
    let perm = delivery_outcome(&["telegram: HTTP 400 (permanent error)".to_string()]);
    assert!(matches!(perm, Delivery::Skipped(_)));
    let n = pass_with(&mut st, &dev, &mut sent, &|id| if id == 12 { perm.clone() } else { Delivery::Delivered });
    assert_eq!(n, 3);
    assert_eq!(sent, vec![11, 12, 13]);
    assert_eq!(st.load().0, 13, "moved past the skipped one");
}

/// Link drops in the middle: held at the failing SMS, resumes from it.
#[test]
fn sms_deferred_midway_resumes() {
    let mut dev = FakeDev::new(1..=10);
    let mut st = Persisted::new(10, Some(0));
    dev.add(11..=13);
    let mut sent = Vec::new();
    let n = pass_with(&mut st, &dev, &mut sent, &|id| {
        if id >= 12 { delivery_outcome(&["slack: io: connection refused".to_string()]) } else { Delivery::Delivered }
    });
    assert_eq!(n, 1);
    assert_eq!(st.load().0, 11, "held before 12");
    assert_eq!(pass_with(&mut st, &dev, &mut sent, &|_| Delivery::Delivered), 2);
    once_each(&sent, 11..=13);
}

#[test]
fn delivery_outcome_classifies() {
    assert_eq!(delivery_outcome(&[]), Delivery::Delivered);
    for e in [NO_WAN_ERR, NO_SERVICE_ERR, "discord: HTTP 502", "telegram: HTTP 429 rate limited", "x (failed after 3 retries)"] {
        assert!(matches!(delivery_outcome(&[e.to_string()]), Delivery::Deferred(_)), "{e}");
    }
    let mixed = ["a: HTTP 404 (permanent error)".to_string(), "b: HTTP 500".to_string()];
    assert!(matches!(delivery_outcome(&mixed), Delivery::Deferred(_)), "any transient holds");
}

#[test]
fn deferred_retries_update_one_log_line() {
    let e = |ok: bool, rule: u32| ForwardLogEntry {
        timestamp: 0, sms_id: 7, sender: String::new(), content_preview: String::new(), rule_name: String::new(),
        destination_type: String::new(), success: ok, error: None, rule_id: rule, content: String::new(), date: String::new(),
    };
    let mut log = Vec::new();
    push_log(&mut log, e(false, 1));
    push_log(&mut log, e(false, 1));
    push_log(&mut log, e(false, 2));
    assert_eq!(log.len(), 2);
    push_log(&mut log, e(true, 1));
    assert_eq!(log.len(), 2);
    assert!(log[0].success);
    push_log(&mut log, e(true, 1));
    assert_eq!(log.len(), 3, "a success is never overwritten");
}

fn once_each(sent: &[u64], want: std::ops::RangeInclusive<u64>) {
    let mut sorted = sent.to_vec();
    sorted.sort_unstable();
    assert_eq!(sorted, want.collect::<Vec<_>>(), "each exactly once");
    assert!(sent.windows(2).all(|w| w[0] < w[1]), "in id order");
}

/// Verify 1: a burst of 600 is paged 50 at a time, all forwarded, no dup.
#[test]
fn sms_burst_600_paged_forwarded_once() {
    let mut dev = FakeDev::new(1..=20);
    let mut st = Persisted::new(20, None);
    dev.add(21..=620);
    let mut sent = Vec::new();
    let mut calls = 0;
    let (wm, lt) = st.load();
    let n = run_pass(
        wm,
        lt,
        dev.max_id(),
        &mut |a, l| dev.list_after(a, l, &mut calls),
        &mut |s: &DecodedSms| {
            sent.push(s.id);
            Delivery::Delivered
        },
        &mut |w, t| st.json = serde_json::to_string(&ForwardState { last_forwarded_id: w, last_forwarded_time: t, log: Vec::new(), held: None }).unwrap(),
    )
    .unwrap();
    assert_eq!(n, 600);
    once_each(&sent, 21..=620);
    assert_eq!(calls, 12, "600 / 50 pages");
    assert_eq!(st.load().0, 620);
    assert!(st.load().1.is_some(), "last forwarded time recorded");
    // Next pass: nothing new, no fetch at all.
    let mut again = Vec::new();
    assert_eq!(st.pass(&dev, None, &mut again).unwrap(), 0);
    assert!(again.is_empty());
}

/// Verify 2: the pass dies on page 3; the restart resumes from the
/// persisted watermark — nothing lost, nothing sent twice.
#[test]
fn sms_interrupted_on_page_3_resumes() {
    let dev = FakeDev::new(1..=600);
    let mut st = Persisted::new(0, None);
    // The forwarder starts with the watermark at 0 only on an empty device;
    // here the device is full of unforwarded SMS on purpose.
    let mut sent = Vec::new();
    assert!(st.pass(&dev, Some(3), &mut sent).is_err());
    assert_eq!(sent.len(), 100, "pages 1–2 done");
    assert_eq!(st.load().0, 100, "watermark moved item by item");
    // Restart (fresh state read from the file).
    let mut st = Persisted { json: st.json.clone() };
    st.pass(&dev, None, &mut sent).unwrap();
    once_each(&sent, 1..=600);

    // Sent/draft rows move the cursor too (shared id counter), so a run
    // of them can never pin the watermark.
    let mut dev = FakeDev::new(1..=10);
    dev.add(11..=130);
    dev.sent_tags = (11..=120).collect();
    let mut st = Persisted::new(10, None);
    let mut sent = Vec::new();
    st.pass(&dev, None, &mut sent).unwrap();
    once_each(&sent, 121..=130);
    assert_eq!(st.load().0, 130);
}

/// Verify 3: core is down while 120 SMS arrive and the direct read also
/// fails part way (one store); back on core, the source-switch pass
/// forwards all 120, once.
#[test]
fn sms_core_outage_120_backfilled() {
    let mut dev = FakeDev::new(1..=100);
    let mut st = Persisted::new(100, Some(0));
    let mut sched = PollSchedule::new(Duration::ZERO, true);
    assert!(sched.source_changed(false));
    dev.add(101..=220);
    let mut sent = Vec::new();
    // Direct read with the SIM store failing: nothing moves (R15).
    let (wm, lt) = st.load();
    let r = run_pass(
        wm,
        lt,
        dev.max_id(),
        &mut |a, l| {
            list_after_direct(a, l, &mut |store, page| {
                if store == 0 {
                    return Err("timeout".into());
                }
                dev.store_page(&json!({"page": page, "data_per_page": LIST_PAGE, "mem_store": store, "order_by": "order by id desc"}))
            })
        },
        &mut |s: &DecodedSms| {
            sent.push(s.id);
            Delivery::Delivered
        },
        &mut |_, _| panic!("a failed read must not commit"),
    );
    assert!(r.unwrap_err().contains("mem_store 0"));
    assert!(sent.is_empty());
    // core back: switch to core and page through.
    assert!(sched.source_changed(true));
    st.pass(&dev, None, &mut sent).unwrap();
    once_each(&sent, 101..=220);
    assert_eq!(st.load().0, 220);
}

/// Rollback through the pager: the whole list is read (every page OK)
/// before plan_forward decides; newer-than-last-time only.
#[test]
fn sms_rollback_pages_whole_list() {
    let dev = FakeDev::new(1..=120);
    let t = |id: u64| sms_time_key(&format!("26,08,27,{:02},{:02},00,+32", (id / 60) % 24, id % 60));
    let mut st = Persisted::new(5000, t(110));
    let mut sent = Vec::new();
    assert!(st.pass(&dev, Some(2), &mut sent).is_err(), "page 2 fails");
    assert!(sent.is_empty());
    assert_eq!(st.load(), (5000, t(110)), "nothing moved");
    st.pass(&dev, None, &mut sent).unwrap();
    once_each(&sent, 111..=120);
    assert_eq!(st.load().0, 120);
}

/// u60-policy's `run_pass` (over ids and time keys only) does what this one does: random
/// device lists, watermarks, last times, page outcomes and delivery outcomes; the same calls,
/// commits and result (the policy contract).
#[test]
fn u60_policy_run_pass_matches() {
    use luyaz_policy::sms_forward as pol;
    let mut seed: u64 = 0x5e5_f0e;
    let mut rnd = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n.max(1)
    };
    let to_item = |v: &Value| {
        let (id, sms) = decode_item(v);
        pol::Item { id, received: sms.is_some(), time: sms.and_then(|m| sms_time_key(&m.date)) }
    };
    let (mut rollbacks, mut deferred, mut sent_total, mut errors) = (0, 0, 0, 0);
    for _ in 0..3000 {
        let n = rnd(140);
        let base = rnd(400);
        let rows: Vec<Value> = (0..n)
            .map(|k| {
                let id = base + k * (1 + rnd(3)) + 1;
                let date = match rnd(6) {
                    0 => String::new(),
                    1 => "garbage".to_string(),
                    _ => format!("{},{:02},{:02},{:02},{:02},{:02},+{}", 24 + rnd(3), 1 + rnd(12), 1 + rnd(28), rnd(24), rnd(60), rnd(60), rnd(9)),
                };
                json!({"id": id, "tag": rnd(4), "number": "x", "content": "y", "date": date})
            })
            .collect();
        let max_id = rows.iter().map(item_id).max().unwrap_or(0);
        let watermark = match rnd(4) {
            0 => max_id,
            1 => max_id + 1 + rnd(50),
            _ => rnd(max_id + 1),
        };
        let last_time = if rnd(3) == 0 { None } else { rows.get(rnd(n.max(1)) as usize).and_then(|r| sms_time_key(r["date"].as_str().unwrap_or(""))) };
        let fail_after = if rnd(5) == 0 { Some(rnd(4)) } else { None };
        let outcomes: Vec<u64> = (0..200).map(|_| rnd(10)).collect();
        let page = |after: u64, limit: u64, calls: &mut u64| -> Result<Vec<Value>, String> {
            *calls += 1;
            if fail_after.is_some_and(|f| *calls > f) {
                return Err("busy".into());
            }
            let mut v: Vec<Value> = rows.iter().filter(|r| item_id(r) > after).cloned().collect();
            v.sort_by_key(item_id);
            v.truncate(limit as usize + 1);
            Ok(v)
        };
        let delivery = |i: usize| match outcomes[i % outcomes.len()] {
            0 => Delivery::Deferred("HTTP 502".into()),
            1 => Delivery::Skipped("HTTP 404 (permanent error)".into()),
            _ => Delivery::Delivered,
        };
        let pol_delivery = |i: usize| match outcomes[i % outcomes.len()] {
            0 => pol::Delivery::Deferred("HTTP 502".into()),
            1 => pol::Delivery::Skipped("HTTP 404 (permanent error)".into()),
            _ => pol::Delivery::Delivered,
        };
        // this crate's
        let (mut calls, mut fwd, mut commits) = (0, Vec::new(), Vec::new());
        let a = run_pass(
            watermark,
            last_time,
            max_id,
            &mut |after, limit| {
                let mut v = page(after, limit, &mut calls)?;
                let has_more = v.len() as u64 > limit;
                v.truncate(limit as usize);
                Ok(Page { items: v, has_more })
            },
            &mut |m| {
                fwd.push(m.id);
                delivery(fwd.len() - 1)
            },
            &mut |w, t| commits.push((w, t)),
        );
        // u60-policy's
        let (mut calls2, mut fwd2, mut commits2) = (0, Vec::new(), Vec::new());
        let b = pol::run_pass(
            watermark,
            last_time,
            max_id,
            &mut |after, limit| {
                let mut v = page(after, limit, &mut calls2)?;
                let has_more = v.len() as u64 > limit;
                v.truncate(limit as usize);
                Ok(pol::Page { items: v.iter().map(to_item).collect(), has_more })
            },
            &mut |m| {
                fwd2.push(m.id);
                pol_delivery(fwd2.len() - 1)
            },
            &mut |w, t| commits2.push((w, t)),
        );
        assert_eq!(a, b, "result: wm {watermark} lt {last_time:?} max {max_id}");
        assert_eq!((calls, &fwd, &commits), (calls2, &fwd2, &commits2), "wm {watermark} lt {last_time:?} max {max_id}");
        rollbacks += (max_id < watermark && max_id > 0) as u32;
        deferred += fwd.iter().enumerate().any(|(i, _)| outcomes[i % outcomes.len()] == 0) as u32;
        sent_total += a.as_ref().map_or(0, |n| *n);
        errors += a.is_err() as u32;
    }
    assert!(rollbacks > 300 && deferred > 300 && sent_total > 5_000 && errors > 50, "{rollbacks} {deferred} {sent_total} {errors}");
}

/// Pass records as u60d gets them: pages, forwards and commits parse into u60-policy's types,
/// and its run_pass answered from the record makes the same commits (the replay contract).
#[test]
fn pass_records_replay_in_u60_policy() {
    use luyaz_policy::sms_forward as pol;
    let rows: Vec<Value> = (1..=130)
        .map(|id| {
            let date = if id % 11 == 0 { String::new() } else { format!("26,09,{:02},10,00,00,+8", 1 + id % 28) };
            json!({"id": id, "tag": if id % 7 == 0 { 2 } else { 0 }, "number": "x", "content": "secret body", "date": date})
        })
        .collect();
    for (wm, max_id, defer_at) in [(20, 130, None), (20, 130, Some(3)), (200, 130, None), (130, 130, None)] {
        let mut n = 0;
        let (res, rec) = record_pass(
            "direct",
            wm,
            Some(1_790_000_000),
            max_id,
            &mut |after, limit| {
                let mut v: Vec<Value> = rows.iter().filter(|r| item_id(r) > after).cloned().collect();
                let has_more = v.len() as u64 > limit;
                v.truncate(limit as usize);
                Ok(Page { items: v, has_more })
            },
            &mut |_| {
                n += 1;
                if Some(n) == defer_at { Delivery::Deferred("HTTP 502".into()) } else { Delivery::Delivered }
            },
            &mut |_, _| {},
        );
        let Some(rec) = rec else {
            assert_eq!(wm, max_id, "only a pass with nothing to do leaves no record");
            continue;
        };
        let text = rec.to_string();
        assert!(!text.contains("secret body") && !text.contains("\"x\""), "no sender or body");
        let pages: Vec<pol::Page> = rec["pages"].as_array().unwrap().iter().map(|p| serde_json::from_value(p["page"].clone()).unwrap()).collect();
        let outcomes: Vec<pol::Delivery> = rec["forwards"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| match f["outcome"].as_str().unwrap() {
                "deferred" => pol::Delivery::Deferred(String::new()),
                "skipped" => pol::Delivery::Skipped(String::new()),
                _ => pol::Delivery::Delivered,
            })
            .collect();
        let (mut pi, mut fi, mut commits) = (pages.into_iter(), outcomes.into_iter(), Vec::new());
        let r = pol::run_pass(
            rec["watermark"].as_u64().unwrap(),
            rec["last_time"].as_i64(),
            rec["max_id"].as_u64().unwrap(),
            &mut |_, _| pi.next().ok_or_else(|| "no page".to_string()),
            &mut |_| fi.next().unwrap(),
            &mut |w, t| commits.push(json!([w, t])),
        );
        assert_eq!(r, res, "{wm} {max_id}");
        assert_eq!(Value::from(commits), rec["commits"], "{wm} {max_id}");
    }
}

const T0: i64 = 1_000_000;

#[test]
fn normal_increasing_ids_forward_only_new() {
    let s = snap(103, &[(99, Some(T0 - 20)), (100, Some(T0)), (101, Some(T0 + 10)), (103, Some(T0 + 30))]);
    let p = plan_forward(100, Some(T0), Ok(&s));
    assert!(!p.rollback);
    assert_eq!(p.forward_ids, vec![101, 103]);
    assert_eq!(p.new_watermark, 103);
    assert_eq!(p.new_last_time, Some(T0 + 30));
}

#[test]
fn nothing_new_changes_nothing() {
    let s = snap(100, &[(99, Some(T0 - 20)), (100, Some(T0))]);
    let p = plan_forward(100, Some(T0), Ok(&s));
    assert_eq!(p, ForwardPlan { forward_ids: vec![], new_watermark: 100, new_last_time: Some(T0), rollback: false });
}

#[test]
fn rollback_forwards_ids_1_and_2() {
    // watermark 100, device numbering reset, new SMS 1 and 2 arrive
    let s = snap(2, &[(2, Some(T0 + 60)), (1, Some(T0 + 30))]);
    let p = plan_forward(100, Some(T0), Ok(&s));
    assert!(p.rollback);
    assert_eq!(p.forward_ids, vec![1, 2]);
    assert_eq!(p.new_watermark, 2);
    assert_eq!(p.new_last_time, Some(T0 + 60));
    // next round: nothing is re-sent
    let p2 = plan_forward(p.new_watermark, p.new_last_time, Ok(&s));
    assert!(!p2.rollback);
    assert!(p2.forward_ids.is_empty());
    // and SMS 3 goes through the normal path
    let s3 = snap(3, &[(1, Some(T0 + 30)), (2, Some(T0 + 60)), (3, Some(T0 + 90))]);
    let p3 = plan_forward(p.new_watermark, p.new_last_time, Ok(&s3));
    assert_eq!(p3.forward_ids, vec![3]);
    assert_eq!(p3.new_watermark, 3);
}

#[test]
fn rollback_does_not_resend_old_sms() {
    // after the reset the device still holds older messages (time <= last forwarded)
    let s = snap(4, &[(1, Some(T0 - 500)), (2, Some(T0)), (3, None), (4, Some(T0 + 5))]);
    let p = plan_forward(100, Some(T0), Ok(&s));
    assert!(p.rollback);
    assert_eq!(p.forward_ids, vec![4]);
    assert_eq!(p.new_watermark, 4);
    assert_eq!(p.new_last_time, Some(T0 + 5));

    let only_old = snap(2, &[(1, Some(T0 - 500)), (2, Some(T0 - 1))]);
    let p = plan_forward(100, Some(T0), Ok(&only_old));
    assert!(p.rollback);
    assert!(p.forward_ids.is_empty());
    assert_eq!(p.new_watermark, 2);
    assert_eq!(p.new_last_time, Some(T0));
}

#[test]
fn one_store_error_no_rollback_no_advance_no_forward() {
    // NV store answers with only low ids, SIM store fails
    let res = merge_store_results(vec![
        (1, Ok(json!({"messages": [{"id": "1", "tag": "0"}, {"id": 2, "tag": 1}]}))),
        (0, Err("ubus timeout".to_string())),
    ]);
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(err.contains("mem_store 0"), "{err}");
    let p = plan_forward(100, Some(T0), Err(&err));
    assert_eq!(p, ForwardPlan { forward_ids: vec![], new_watermark: 100, new_last_time: Some(T0), rollback: false });

    let res = merge_store_results(vec![(1, Err("boom".into())), (0, Ok(json!({"messages": []})))]);
    assert!(res.is_err());
}

#[test]
fn both_stores_ok_merges_and_dedups() {
    let res = merge_store_results(vec![
        (1, Ok(json!({"messages": [{"id": "5"}, {"id": 3}]}))),
        (0, Ok(json!({"messages": [{"id": 5}, {"id": "4"}]}))),
        (0, Ok(json!({}))),
    ])
    .unwrap();
    let ids: Vec<u64> = res.iter().map(item_id).collect();
    assert_eq!(ids, vec![5, 3, 4]);
}

#[test]
fn rollback_without_last_time_is_conservative() {
    // old state file: no time recorded -> forward nothing, rebase on device
    let s = snap(2, &[(1, Some(T0 + 30)), (2, Some(T0 + 60))]);
    let p = plan_forward(100, None, Ok(&s));
    assert!(p.rollback);
    assert!(p.forward_ids.is_empty());
    assert_eq!(p.new_watermark, 2);
    assert_eq!(p.new_last_time, Some(T0 + 60));
}

#[test]
fn missing_last_time_backfilled_from_covered_messages() {
    let s = snap(102, &[(99, Some(T0 - 10)), (100, Some(T0)), (102, Some(T0 + 20))]);
    let p = plan_forward(100, None, Ok(&s));
    assert!(!p.rollback);
    assert_eq!(p.forward_ids, vec![102]);
    assert_eq!(p.new_last_time, Some(T0 + 20));
    let s = snap(100, &[(99, Some(T0 - 10)), (100, Some(T0))]);
    assert_eq!(plan_forward(100, None, Ok(&s)).new_last_time, Some(T0));
}

#[test]
fn empty_device_is_not_a_rollback() {
    let p = plan_forward(100, Some(T0), Ok(&snap(0, &[])));
    assert!(!p.rollback);
    assert_eq!(p.new_watermark, 100);
    assert!(p.forward_ids.is_empty());
}

#[test]
fn old_state_file_without_time_still_loads() {
    let st: ForwardState = serde_json::from_str(r#"{"last_forwarded_id":42,"log":[]}"#).unwrap();
    assert_eq!(st.last_forwarded_id, 42);
    assert_eq!(st.last_forwarded_time, None);
    let st: ForwardState = serde_json::from_str(r#"{"last_forwarded_id":42}"#).unwrap();
    assert_eq!(st.last_forwarded_time, None);
    let back = serde_json::to_string(&ForwardState { last_forwarded_id: 7, last_forwarded_time: Some(5), log: vec![], held: None }).unwrap();
    let st: ForwardState = serde_json::from_str(&back).unwrap();
    assert_eq!((st.last_forwarded_id, st.last_forwarded_time), (7, Some(5)));
}

#[test]
fn sms_time_key_orders_and_uses_zone() {
    let a = sms_time_key("26,09,25,13,20,00,+8").unwrap();
    let b = sms_time_key("26;09;25;13;20;01;+8").unwrap();
    assert_eq!(b - a, 1);
    // same instant in another zone
    assert_eq!(sms_time_key("26,09,25,05,20,00,+0").unwrap(), a);
    // quarter-hour zone form (+32 = +8h)
    assert_eq!(sms_time_key("26,09,25,13,20,00,+32").unwrap(), a);
    // day / month / year boundaries keep ordering
    assert!(sms_time_key("26,10,01,00,00,00,+8").unwrap() > sms_time_key("26,09,30,23,59,59,+8").unwrap());
    assert!(sms_time_key("27,01,01,00,00,00,+8").unwrap() > sms_time_key("26,12,31,23,59,59,+8").unwrap());
    assert_eq!(sms_time_key("00,01,01,00,00,00"), Some(0));
    assert_eq!(sms_time_key("00,03,01,00,00,00"), Some((31 + 29) * 86_400));
    assert_eq!(sms_time_key("garbage"), None);
    assert_eq!(sms_time_key("26,13,01,00,00,00"), None);
}

fn row(id: u64, tag: &str, date: &str) -> Value {
    json!({"id": id.to_string(), "number": "0031003000300038003600", "content": "0041", "tag": tag, "date": date})
}

/// One pass over a fixed list (ascending, one page).
fn pass_rows(wm: u64, lt: Option<i64>, rows: &[Value]) -> (Vec<u64>, u64, Option<i64>) {
    let max_id = rows.iter().map(item_id).max().unwrap_or(0);
    let mut sent = Vec::new();
    let mut last = (wm, lt);
    run_pass(
        wm,
        lt,
        max_id,
        &mut |after, _| Ok(Page { items: rows.iter().filter(|r| item_id(r) > after).cloned().collect(), has_more: false }),
        &mut |s: &DecodedSms| {
            sent.push(s.id);
            Delivery::Delivered
        },
        &mut |w, t| last = (w, t),
    )
    .unwrap();
    (sent, last.0, last.1)
}

/// Card swap: the SIM's years-old SMS come back with ids above the
/// watermark. They are skipped, the watermark moves past them, and a
/// really new SMS after them still goes out.
#[test]
fn sms_reimported_sim_messages_skipped() {
    let lt = sms_time_key("26,10,05,12,12,33,+32");
    let rows = [
        row(254, "0", "23,02,08,08,14,46,+32"),
        row(255, "0", "23,02,10,16,37,23,+32"),
        row(256, "1", "23,04,03,14,00,25,+32"),
        row(257, "0", "26,10,05,13,00,00,+32"),
    ];
    let (sent, wm, new_lt) = pass_rows(253, lt, &rows);
    assert_eq!(sent, vec![257]);
    assert_eq!(wm, 257);
    assert_eq!(new_lt, sms_time_key("26,10,05,13,00,00,+32"));
}

/// Mail held by the SMS centre, or a sender in another zone, dated a bit
/// before the last forwarded SMS: still forwarded.
#[test]
fn sms_late_or_other_zone_still_forwarded() {
    let lt = sms_time_key("26,10,05,12,00,00,+32");
    let rows = [
        row(11, "0", "26,10,03,12,00,00,+32"),  // two days late
        row(12, "0", "26,10,05,00,00,00,-40"),  // UTC-10, i.e. later
        row(13, "0", "26,09,30,12,00,00,+32"),  // just inside 7 days
    ];
    let (sent, wm, _) = pass_rows(10, lt, &rows);
    assert_eq!(sent, vec![11, 12, 13]);
    assert_eq!(wm, 13);
}

/// Without a time baseline nothing is judged old.
#[test]
fn sms_no_last_time_never_skips() {
    let (sent, _, _) = pass_rows(10, None, &[row(11, "0", "20,01,01,00,00,00,+32")]);
    assert_eq!(sent, vec![11]);
}

/// Baseline = highest id of any tag, newest time of received SMS only.
#[test]
fn sms_baseline_of_rows() {
    let rows = [
        row(253, "1", "26,10,05,12,12,33,+32"),
        row(252, "2", "26,10,06,00,00,00,+32"), // sent: time ignored
        row(257, "0", "23,04,04,08,04,30,+32"),
    ];
    assert_eq!(baseline_of(&rows), (257, sms_time_key("26,10,05,12,12,33,+32")));
    assert_eq!(baseline_of(&[]), (0, None));
}

fn rule(enabled: bool) -> ForwardRule {
    serde_json::from_value(json!({"id": 1, "name": "r", "enabled": enabled,
        "filter": {"type": "all"}, "destination": {"type": "sms", "forward_number": "10086"}}))
    .unwrap()
}

/// Switching forwarding on (master switch, or the first rule) drops the
/// stale baseline so the next check re-reads it from the device; other
/// changes leave it alone.
#[test]
fn sms_switch_on_resets_baseline() {
    let (fwd, _host, _dir) = forwarder("switch-on", None, None);
    let set = |wm: u64| {
        let mut st = fwd.state.lock().unwrap();
        st.last_forwarded_id = wm;
        st.last_forwarded_time = Some(5);
    };
    let wm = || fwd.state.lock().unwrap().last_forwarded_id;
    let mut config = SmsForwardConfig::default();

    set(9);
    config.enabled = true; // no rule yet: still not running
    commit_config(&fwd, false, &config);
    assert_eq!(wm(), 9);

    let was = is_active(&config);
    config.rules.push(rule(true));
    commit_config(&fwd, was, &config);
    assert_eq!((wm(), fwd.state.lock().unwrap().last_forwarded_time), (0, None));

    set(300); // running: edits keep the baseline
    let was = is_active(&config);
    config.poll_interval_secs = 60;
    commit_config(&fwd, was, &config);
    assert_eq!(wm(), 300);

    let was = is_active(&config);
    config.rules[0].enabled = false;
    commit_config(&fwd, was, &config);
    assert_eq!(wm(), 300, "switching off keeps it");
    let was = is_active(&config);
    config.rules[0].enabled = true;
    commit_config(&fwd, was, &config);
    assert_eq!(wm(), 0, "switching back on resets it");
}

// ── the engine through a fake host (9e: single owner) ──

/// A device behind [`Host`]: the core's `sms` block over `rows`, vendor sends recorded by number,
/// ubus reads failing (the cleanup after an SMS send just logs), no sleeping.
#[derive(Default)]
struct FakeHost {
    rows: Mutex<Vec<Value>>,
    active: AtomicBool,
    /// Numbers whose send times out; the host is handed over while it waits to retry.
    fail_numbers: Mutex<Vec<String>>,
    /// Handed over right after this many sends went out.
    handover_after_sends: Mutex<Option<usize>>,
    sends: Mutex<Vec<String>>,
    controls: Mutex<Vec<String>>,
    sleeps: AtomicUsize,
    /// A SIM switch holds the checks.
    hold: AtomicBool,
    /// The core's `sms` block stuck at this (max id, count) instead of following `rows`.
    frozen_summary: Mutex<Option<(u64, u64)>>,
    /// Direct `zwrt_wms` SMS reads answer from `rows` (NV store; SIM store empty).
    ubus_sms: AtomicBool,
    /// Numbers whose send fails transiently (no hand-over).
    down_numbers: Mutex<Vec<String>>,
    alerts: Mutex<Vec<String>>,
}

impl Host for FakeHost {
    fn sms_summary(&self) -> Option<(u64, u64)> {
        if let Some(frozen) = *self.frozen_summary.lock().unwrap() {
            return Some(frozen);
        }
        let rows = self.rows.lock().unwrap();
        Some((rows.iter().map(item_id).max().unwrap_or(0), rows.len() as u64))
    }
    fn list_after(&self, after: u64, limit: u64) -> Result<Page, String> {
        let mut v: Vec<Value> = self.rows.lock().unwrap().iter().filter(|r| item_id(r) > after).cloned().collect();
        v.sort_by_key(item_id);
        let has_more = v.len() as u64 > limit;
        v.truncate(limit as usize);
        Ok(Page { items: v, has_more })
    }
    fn ubus_read(&self, obj: &str, method: &str, params: Option<&str>, _: bool) -> Result<Value, String> {
        if self.ubus_sms.load(Ordering::SeqCst) && method == "zte_libwms_get_sms_data" {
            let q: Value = serde_json::from_str(params.unwrap()).unwrap();
            if q["mem_store"] != 1 {
                return Ok(json!({"messages": []}));
            }
            let (page, per) = (q["page"].as_u64().unwrap() as usize, q["data_per_page"].as_u64().unwrap() as usize);
            let mut v = self.rows.lock().unwrap().clone();
            v.sort_by_key(|r| std::cmp::Reverse(item_id(r)));
            let v: Vec<Value> = v.into_iter().skip(page * per).take(per).collect();
            return Ok(json!({"messages": v}));
        }
        Err(format!("{obj} {method}: no ubus here"))
    }
    fn vendor(&self, _: &str, _: &str, params: Option<&str>) -> Result<Value, String> {
        let p: Value = serde_json::from_str(params.unwrap()).unwrap();
        let number = p["number"].as_str().unwrap().to_string();
        if self.down_numbers.lock().unwrap().contains(&number) {
            return Err("io: timed out".into());
        }
        if self.fail_numbers.lock().unwrap().contains(&number) {
            self.active.store(false, Ordering::SeqCst);
            return Err("io: timed out".into());
        }
        let mut sends = self.sends.lock().unwrap();
        sends.push(number);
        if *self.handover_after_sends.lock().unwrap() == Some(sends.len()) {
            self.active.store(false, Ordering::SeqCst);
        }
        Ok(json!({"result": 3}))
    }
    fn control(&self, action: &str, _: &Value) -> Result<Value, String> {
        self.controls.lock().unwrap().push(action.to_string());
        Ok(json!({}))
    }
    fn alert(&self, key: &str, _: &str) {
        self.alerts.lock().unwrap().push(key.to_owned());
    }
    fn tick(&self, _: Value) {}
    fn sms_zone_hours(&self) -> i64 {
        8
    }
    fn active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
    fn sleep(&self, _: Duration) {
        self.sleeps.fetch_add(1, Ordering::SeqCst);
    }
    fn hold(&self) -> bool {
        self.hold.load(Ordering::SeqCst)
    }
}

fn test_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("u60-sms-forward-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// A forwarder on files in a fresh temp dir (`config` / `state` written there first), online.
fn forwarder(name: &str, config: Option<Value>, state: Option<Value>) -> (Arc<SmsForwarder>, Arc<FakeHost>, PathBuf) {
    let d = test_dir(name);
    if let Some(c) = config {
        fs::write(d.join("config.json"), c.to_string()).unwrap();
    }
    if let Some(s) = state {
        fs::write(d.join("state.json"), s.to_string()).unwrap();
    }
    let host = Arc::new(FakeHost { active: AtomicBool::new(true), ..Default::default() });
    let fwd = Arc::new(SmsForwarder::new(host.clone(), d.join("config.json"), d.join("state.json")));
    fwd.set_service("LTE");
    fwd.set_wan("ipv4_connected");
    (fwd, host, d)
}

fn sms_rules(numbers: &[&str]) -> Value {
    let rules: Vec<Value> = numbers
        .iter()
        .enumerate()
        .map(|(i, n)| json!({"id": i + 1, "name": format!("r{}", i + 1), "enabled": true,
            "filter": {"type": "all"}, "destination": {"type": "sms", "forward_number": n}}))
        .collect();
    json!({"enabled": true, "rules": rules})
}

fn sms_row(id: u64) -> Value {
    json!({"id": id, "number": "10086", "content": "hello", "tag": 0, "date": format!("26,10,09,10,{:02},00,+8", id % 60)})
}

fn read_back(d: &Path) -> ForwardState {
    serde_json::from_str(&fs::read_to_string(d.join("state.json")).unwrap()).unwrap()
}

fn queue_of(d: &Path) -> Queue {
    Queue::load(&d.join("sms-forward-queue.json")).expect("queue file").expect("queue parses")
}

/// A queue file that does not parse is moved aside and `.prev` read back: what went out stays
/// sent (not forwarded again), and one alert says so.
#[test]
fn corrupt_queue_recovers_from_prev_without_resending() {
    let (fwd, host, d) = forwarder("corrupt-prev", Some(sms_rules(&["111"])), Some(json!({"last_forwarded_id": 10})));
    host.rows.lock().unwrap().push(sms_row(11));
    fwd.check(host.sms_summary());
    assert_eq!(*host.sends.lock().unwrap(), vec!["111"]);
    // one more save, so `.prev` holds the job as sent (it is one save behind the main file)
    let qp = d.join("sms-forward-queue.json");
    fwd.queue.lock().unwrap().save(&qp).unwrap();
    fs::write(&qp, b"{\"jobs\": [tru").unwrap();
    drop(fwd);

    let fwd2 = Arc::new(SmsForwarder::new(host.clone(), d.join("config.json"), d.join("state.json")));
    fwd2.set_service("LTE");
    fwd2.set_wan("ipv4_connected");
    host.rows.lock().unwrap()[0]["id"] = json!(31); // the firmware renumbered it
    fwd2.check(host.sms_summary());
    assert_eq!(*host.sends.lock().unwrap(), vec!["111"], "sent again after recovery");
    assert_eq!(host.alerts.lock().unwrap().iter().filter(|k| *k == "sms-forward-queue").count(), 1);
    assert!(fs::read_dir(&d).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains(".corrupt-")));
    assert!(queue_of(&d).seeded);
    let _ = fs::remove_dir_all(&d);
}

/// Main file and `.prev` both unusable: rebuilt unseeded, so the SMS already on the device are
/// fingerprinted, not forwarded.
#[test]
fn corrupt_queue_without_prev_reseeds_instead_of_resending() {
    let (fwd, host, d) = forwarder("corrupt-none", Some(sms_rules(&["111"])), Some(json!({"last_forwarded_id": 10})));
    host.rows.lock().unwrap().push(sms_row(5));
    fwd.check(host.sms_summary());
    drop(fwd);
    fs::write(d.join("sms-forward-queue.json"), b"garbage").unwrap();
    let _ = fs::remove_file(d.join("sms-forward-queue.json.prev"));

    let fwd2 = Arc::new(SmsForwarder::new(host.clone(), d.join("config.json"), d.join("state.json")));
    fwd2.set_service("LTE");
    fwd2.set_wan("ipv4_connected");
    fwd2.check(host.sms_summary());
    assert!(host.sends.lock().unwrap().is_empty(), "old SMS forwarded after rebuild");
    assert_eq!(host.alerts.lock().unwrap().as_slice(), ["sms-forward-queue"]);
    assert!(queue_of(&d).seeded, "fingerprinted again");
    let _ = fs::remove_dir_all(&d);
}

/// Restart with a job pending: the next forwarder, built from the same files, sends only what
/// is still pending; the rule that already got the SMS never gets it again.
#[test]
fn restart_resumes_pending_and_never_resends() {
    let (fwd, host, d) = forwarder("resume", Some(sms_rules(&["111", "222"])), Some(json!({"last_forwarded_id": 10})));
    host.rows.lock().unwrap().push(sms_row(11));
    host.down_numbers.lock().unwrap().push("222".into());
    fwd.check(host.sms_summary());
    assert_eq!(*host.sends.lock().unwrap(), vec!["111"]);
    assert_eq!(read_back(&d).last_forwarded_id, 11, "the watermark moved on: queued");
    let q = queue_of(&d);
    let states: Vec<(u32, JobState, u32)> = q.jobs.iter().map(|j| (j.rule_id, j.state, j.attempts)).collect();
    assert_eq!(states, vec![(1, JobState::Sent, 0), (2, JobState::Pending, 1)]);
    assert!(host.controls.lock().unwrap().is_empty());

    let host2 = Arc::new(FakeHost { active: AtomicBool::new(true), ..Default::default() });
    host2.rows.lock().unwrap().push(sms_row(11));
    let fwd2 = SmsForwarder::new(host2.clone(), d.join("config.json"), d.join("state.json"));
    fwd2.set_service("LTE");
    fwd2.check(host2.sms_summary());
    assert!(host2.sends.lock().unwrap().is_empty(), "backing off: not due yet");
    // due now (clock stand-in: pull next_at back)
    fwd2.queue.lock().unwrap().jobs[1].next_at = 0;
    fwd2.check(host2.sms_summary());
    fwd2.check(host2.sms_summary());
    assert_eq!(*host2.sends.lock().unwrap(), vec!["222"], "only the pending one, once");
    assert!(queue_of(&d).jobs.iter().all(|j| j.state == JobState::Sent));
    let _ = fs::remove_dir_all(&d);
}

/// Handed over between two sends of one drain: the rest stays pending for the next owner.
#[test]
fn handover_mid_drain_leaves_the_rest_pending() {
    let (fwd, host, d) = forwarder("between", Some(sms_rules(&["111"])), Some(json!({"last_forwarded_id": 10})));
    host.rows.lock().unwrap().extend([sms_row(11), sms_row(12)]);
    *host.handover_after_sends.lock().unwrap() = Some(1);
    fwd.check(host.sms_summary());
    assert_eq!(host.sends.lock().unwrap().len(), 1);
    let q = queue_of(&d);
    assert_eq!(q.pending().map(|j| j.sms_id).collect::<Vec<_>>(), vec![12]);
    let _ = fs::remove_dir_all(&d);
}

/// The same SMS at a new, higher id (a SIM re-read renumbered it), dated recently: not queued
/// or sent again.
#[test]
fn renumbered_sms_not_resent() {
    let (fwd, host, d) = forwarder("renum", Some(sms_rules(&["111"])), Some(json!({"last_forwarded_id": 10})));
    host.rows.lock().unwrap().push(sms_row(11));
    fwd.check(host.sms_summary());
    let mut again = sms_row(11);
    again["id"] = json!(40);
    host.rows.lock().unwrap().clear();
    host.rows.lock().unwrap().push(again);
    fwd.check(host.sms_summary());
    assert_eq!(host.sends.lock().unwrap().len(), 1);
    assert_eq!(read_back(&d).last_forwarded_id, 40);
    let _ = fs::remove_dir_all(&d);
}

/// First start with the queue: what is already on the device (at or below the watermark) is
/// fingerprinted, so renumbered copies of it are not sent.
#[test]
fn seeding_covers_sms_already_handled() {
    let (fwd, host, d) = forwarder("seed", Some(sms_rules(&["111"])), Some(json!({"last_forwarded_id": 11})));
    host.rows.lock().unwrap().push(sms_row(11));
    fwd.check(host.sms_summary());
    assert!(queue_of(&d).seeded);
    let mut again = sms_row(11);
    again["id"] = json!(30);
    host.rows.lock().unwrap().push(again);
    fwd.check(host.sms_summary());
    assert!(host.sends.lock().unwrap().is_empty());
    let _ = fs::remove_dir_all(&d);
}

/// An old single hold migrates: the rule that already got it is queued as sent.
#[test]
fn old_hold_migrates_into_the_queue() {
    let (fwd, host, d) = forwarder("migrate", Some(sms_rules(&["111", "222"])), Some(json!({"last_forwarded_id": 10, "held": {"sms_id": 11, "first_deferred_at": 0, "online_secs": 5, "delivered_rules": [1]}})));
    host.rows.lock().unwrap().push(sms_row(11));
    fwd.check(host.sms_summary());
    assert_eq!(*host.sends.lock().unwrap(), vec!["222"]);
    assert!(read_back(&d).held.is_none());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn backoff_doubles_to_15_min() {
    let got: Vec<i64> = (1..=8).map(backoff_secs).collect();
    assert_eq!(got, vec![30, 60, 120, 240, 480, 900, 900, 900]);
    assert_eq!(backoff_secs(u32::MAX), 900);
}

fn job(key: &str, rule: u32, state: JobState, next_at: i64, created: i64) -> Job {
    Job { key: key.into(), rule_id: rule, sms_id: 1, sender: String::new(), content: String::new(), date: String::new(), state, attempts: 0, next_at, created_at: created, online_secs: 0, last_error: None }
}

/// Per rule only the oldest pending job runs (order holds), other rules never wait for it; a
/// rule whose link is down sets no timer.
#[test]
fn due_is_per_channel_fifo() {
    let q = Queue { jobs: vec![job("a", 1, JobState::Pending, 500, 0), job("b", 1, JobState::Pending, 0, 0), job("a", 2, JobState::Pending, 0, 0)], ..Default::default() };
    assert_eq!(q.due(100, &|_| true), vec![2]);
    assert_eq!(q.wait_secs(100, &|_| true), Some(0));
    assert_eq!(q.wait_secs(100, &|r| r == 1), Some(400));
    assert_eq!(q.wait_secs(100, &|_| false), None);
}

/// Bounds: give up after 24 h online (reported), finished jobs dropped after 3 days, at most
/// MAX_JOBS kept (finished first).
#[test]
fn queue_bounds() {
    let mut q = Queue::default();
    let mut old = job("x", 1, JobState::Pending, 0, 0);
    old.online_secs = queue::GIVE_UP_ONLINE_SECS;
    q.jobs.push(old);
    q.jobs.push(job("y", 1, JobState::Sent, 0, 0));
    let now = queue::DONE_KEEP_SECS + 10;
    let gone = q.prune(now);
    assert_eq!(gone.len(), 1);
    assert!(q.jobs.is_empty(), "the given-up one is old too");
    for i in 0..queue::MAX_JOBS + 5 {
        q.jobs.push(job(&i.to_string(), 1, JobState::Pending, 0, now));
    }
    q.jobs.insert(0, job("done", 1, JobState::Sent, 0, now));
    let gone = q.prune(now);
    assert_eq!(q.jobs.len(), queue::MAX_JOBS);
    assert_eq!(gone.len(), 5, "the sent one went first, then the oldest pending");
    assert!(!q.jobs.iter().any(|j| j.key == "done"));
}

#[test]
fn fingerprint_is_stable() {
    assert_eq!(fingerprint("a", "b", "c"), fingerprint("a", "b", "c"));
    assert_ne!(fingerprint("ab", "", "c"), fingerprint("a", "b", "c"));
}

/// The loop's start re-reads what another owner left; online time counted here for the same
/// held SMS is kept.
#[test]
fn reload_takes_the_other_owners_state() {
    let (fwd, _host, d) = forwarder("reload", None, Some(json!({"last_forwarded_id": 10, "held": {"sms_id": 11, "first_deferred_at": 0, "online_secs": 50, "delivered_rules": [1]}})));
    fwd.state.lock().unwrap().held.as_mut().unwrap().online_secs = 70;
    fs::write(d.join("state.json"), json!({"last_forwarded_id": 12, "last_forwarded_time": 5, "held": {"sms_id": 13, "first_deferred_at": 0, "online_secs": 9, "delivered_rules": [2]}}).to_string()).unwrap();
    fwd.reload_state();
    let st = fwd.state.lock().unwrap().clone();
    assert_eq!((st.last_forwarded_id, st.last_forwarded_time), (12, Some(5)));
    assert_eq!(st.held.as_ref().unwrap().online_secs, 9, "another SMS: its own time");
    assert_eq!(*fwd.delivered.lock().unwrap(), HashSet::from([(13, 2)]));
    fs::write(d.join("state.json"), json!({"last_forwarded_id": 12, "held": {"sms_id": 13, "first_deferred_at": 0, "online_secs": 4, "delivered_rules": [2]}}).to_string()).unwrap();
    fwd.reload_state();
    assert_eq!(fwd.state.lock().unwrap().held.as_ref().unwrap().online_secs, 9, "same SMS: the larger");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn lease_waits_only_for_another_live_owner() {
    let d = test_dir("lease");
    let path = d.join("owner");
    let host = FakeHost::default();
    let max = Duration::from_secs(3);
    assert!(wait_lease_free(&path, &host, max), "no file");
    write_lease(&path, "test").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), format!("{} test\n", std::process::id()));
    assert!(wait_lease_free(&path, &host, max), "our own");
    fs::write(&path, "4000000000 u60d\n").unwrap();
    assert!(wait_lease_free(&path, &host, max), "dead pid");
    assert_eq!(host.sleeps.load(Ordering::SeqCst), 0);
    fs::write(&path, "1 u60d\n").unwrap(); // pid 1 is always alive
    assert!(!wait_lease_free(&path, &host, max), "live owner held it throughout");
    assert_eq!(host.sleeps.load(Ordering::SeqCst), 3, "1 s steps up to max");
    remove_lease(&path);
    assert!(path.exists(), "not ours: left alone");
    write_lease(&path, "test").unwrap();
    remove_lease(&path);
    assert!(!path.exists());
    let _ = fs::remove_dir_all(&d);
}

/// Held by the host (a SIM switch): nothing sent, read or deleted, the watermark stays; the next
/// check after the hold sends it.
#[test]
fn host_hold_defers_then_sends() {
    let (fwd, host, d) = forwarder("hold", Some(sms_rules(&["111"])), Some(json!({"last_forwarded_id": 10})));
    host.rows.lock().unwrap().push(sms_row(11));
    host.hold.store(true, Ordering::SeqCst);
    fwd.check(host.sms_summary());
    assert!(host.sends.lock().unwrap().is_empty());
    assert!(host.controls.lock().unwrap().is_empty());
    assert_eq!(read_back(&d).last_forwarded_id, 10);
    host.hold.store(false, Ordering::SeqCst);
    fwd.check(host.sms_summary());
    assert_eq!(*host.sends.lock().unwrap(), vec!["111"]);
    assert_eq!(read_back(&d).last_forwarded_id, 11);
    let _ = fs::remove_dir_all(&d);
}

/// The core's `sms` block stops following the device (its ubus listen missed the new SMS, or it
/// went quiet under the rate limit): the summary still says the watermark, yet a second SMS
/// arrived. Seeing the same summary twice must read ubus directly, else only the first SMS
/// is ever forwarded.
#[test]
fn frozen_core_summary_falls_back_to_direct_read() {
    let (fwd, host, d) = forwarder("frozen", Some(sms_rules(&["111"])), Some(json!({"last_forwarded_id": 10})));
    host.ubus_sms.store(true, Ordering::SeqCst);
    host.rows.lock().unwrap().push(sms_row(11));
    fwd.check(host.sms_summary());
    assert_eq!(host.sends.lock().unwrap().len(), 1, "first SMS via core");
    *host.frozen_summary.lock().unwrap() = Some((11, 1));
    host.rows.lock().unwrap().push(sms_row(12));
    fwd.check(host.sms_summary());
    fwd.check(host.sms_summary());
    assert_eq!(host.sends.lock().unwrap().len(), 2, "second SMS found by the direct read");
    assert_eq!(read_back(&d).last_forwarded_id, 12);
    let _ = fs::remove_dir_all(&d);
}

/// A bad SMS date far in the future must not become the last forwarded time: every later SMS
/// would then look "older than the last one" and be skipped as re-read from the SIM.
#[test]
fn future_last_time_does_not_block_new_sms() {
    let (fwd, host, d) = forwarder("future", Some(sms_rules(&["111"])), Some(json!({"last_forwarded_id": 10})));
    let mut bad = sms_row(11);
    bad["date"] = json!("99,12,31,23,59,59,+8");
    host.rows.lock().unwrap().push(bad);
    fwd.check(host.sms_summary());
    host.rows.lock().unwrap().push(sms_row(12));
    fwd.check(host.sms_summary());
    assert_eq!(host.sends.lock().unwrap().len(), 2, "the SMS after the bad date still goes out");
    let _ = fs::remove_dir_all(&d);
}

/// A state file already carrying a future last time (written before the clamp) is ignored.
#[test]
fn saved_future_last_time_is_ignored() {
    let far = sms_time_key("99,12,31,23,59,59,+8").unwrap();
    let (fwd, host, d) = forwarder(
        "saved-future",
        Some(sms_rules(&["111"])),
        Some(json!({"last_forwarded_id": 10, "last_forwarded_time": far})),
    );
    host.rows.lock().unwrap().push(sms_row(11));
    fwd.check(host.sms_summary());
    assert_eq!(host.sends.lock().unwrap().len(), 1);
    let _ = fs::remove_dir_all(&d);
}

/// 「只收到一次」: one channel failing transiently must not stop the others. Rule 2's target is
/// down; SMS 11, 12, 13 arrive: rule 1 gets all three at once, rule 2 none yet.
#[test]
fn one_channel_down_does_not_block_the_others() {
    let (fwd, host, d) = forwarder("hol", Some(sms_rules(&["111", "222"])), Some(json!({"last_forwarded_id": 10})));
    host.down_numbers.lock().unwrap().push("222".into());
    host.rows.lock().unwrap().extend([sms_row(11), sms_row(12), sms_row(13)]);
    fwd.check(host.sms_summary());
    let to = |n: &str| host.sends.lock().unwrap().iter().filter(|s| *s == n).count();
    assert_eq!(to("111"), 3, "rule 1 got every SMS");
    assert_eq!(to("222"), 0);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn late_forward_says_so_in_the_chat_text() {
    let mut m = DecodedSms {
        id: 1,
        sender: "10086".into(),
        content: "hi".into(),
        date: "26,10,27,09,05,00,+8".into(),
        late_secs: None,
    };
    assert!(!send::chat_text(&m).contains("late"));
    m.late_secs = Some(3 * 3600 + 5 * 60);
    assert!(send::chat_text(&m).contains("(late: forwarded 3 h 05 min after it came in)"));
    assert_eq!(send::wait_text(30), "1 min");
    assert_eq!(send::wait_text(45 * 60), "45 min");
    assert_eq!(send::wait_text(50 * 3600), "2 d 2 h");
}

#[test]
fn queue_status_reports_the_oldest_pending_wait() {
    let q = queue::Queue::default();
    assert!(q.status()["oldest_pending_secs"].is_null());
}
