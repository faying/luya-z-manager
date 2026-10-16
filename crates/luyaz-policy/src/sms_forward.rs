//! SMS forwarding's bookkeeping, moved from zte-agent `src/sms_forward.rs` (Phase 4 step 6a).
//!
//! Which SMS to forward and how far the watermark moves ([`plan_forward`], [`run_pass`]), what a
//! set of rule results means for the SMS ([`delivery_outcome`]), and the hold on an SMS a rule
//! could not get yet ([`settle_hold`], [`OnlineClock`]). Reading the SMS list, matching rules
//! against sender and body, sending, logging and saving stay in the host.
//!
//! Deliberate differences from the agent:
//! - An SMS is an [`Item`]: its id, whether it was received, and the time key of its date (the
//!   agent's `sms_time_key`). Sender and body are secret and never cross into the policy: every decision
//!   here reads only ids and times.
//! - `run_pass`'s `forward` gets the [`Item`] instead of the decoded SMS; `is_reimported` takes
//!   the time key. No logging.
//!
//! zte-agent's tests compare its own `run_pass` with this one on random lists (contract test).

/// Page size for `sms.list_after` and the direct reads (datad's limit cap).
pub const LIST_PAGE: u64 = 50;
/// How far before the last handled SMS a received one may be dated and still count as new.
pub const REIMPORT_MARGIN_SECS: i64 = 7 * 86_400;
/// A held SMS that still has not gone out after this much online time is skipped for the rules
/// still failing.
pub const HOLD_TIMEOUT_SECS: u64 = 6 * 3600;
pub const HOLD_TIMEOUT_NOTE: &str = "等待超时跳过";

/// One row of the device's SMS list.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Item {
    pub id: u64,
    /// Received (tag 0/1), not sent or draft.
    pub received: bool,
    /// The time key of its date (the agent's `sms_time_key`); `None` when it has none or it does
    /// not parse. Only read for received ones.
    pub time: Option<i64>,
}

impl Item {
    /// `Some(time)` for a received SMS, as the agent's `decode_item` gives one.
    pub fn received_time(&self) -> Option<Option<i64>> {
        self.received.then_some(self.time)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Page {
    pub items: Vec<Item>,
    pub has_more: bool,
}

/// What happened to one SMS.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Delivery {
    /// Every matching rule succeeded (or none matched): move past it.
    Delivered,
    /// Some rule failed permanently (4xx / config): logged, move past it.
    Skipped(String),
    /// Some rule failed transiently: stop here, resume from this SMS next time.
    Deferred(String),
}

impl Delivery {
    pub fn kind(&self) -> &'static str {
        match self {
            Delivery::Delivered => "delivered",
            Delivery::Skipped(_) => "skipped",
            Delivery::Deferred(_) => "deferred",
        }
    }
}

/// HTTP 4xx are permanent — everything else is transient.
pub fn is_transient_error(err: &str) -> bool {
    !err.contains("(permanent error)")
}

/// Rule errors of one SMS → its outcome. Any transient error wins.
pub fn delivery_outcome(errors: &[String]) -> Delivery {
    if let Some(e) = errors.iter().find(|e| is_transient_error(e)) {
        return Delivery::Deferred(e.clone());
    }
    match errors.first() {
        Some(e) => Delivery::Skipped(e.clone()),
        None => Delivery::Delivered,
    }
}

/// A deferred SMS: when it was first held, how long it has waited while online, and which rules
/// already got it.
#[derive(Clone, Default, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HeldSms {
    pub sms_id: u64,
    pub first_deferred_at: i64,
    #[cfg_attr(feature = "serde", serde(default))]
    pub online_secs: u64,
    #[cfg_attr(feature = "serde", serde(default))]
    pub delivered_rules: Vec<u32>,
}

/// Counts time only while online (see the agent).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OnlineClock {
    pub last: Option<(u64, bool)>,
}

impl OnlineClock {
    pub fn observe(&mut self, now: u64, online: bool) -> u64 {
        let add = match self.last {
            Some((t, true)) => now.saturating_sub(t),
            _ => 0,
        };
        self.last = Some((now, online));
        add
    }
}

/// Settle the hold after one delivery attempt of `sms_id` (see the agent). Returns the outcome
/// and the alert text when the hold timed out.
pub fn settle_hold(
    held: &mut Option<HeldSms>,
    sms_id: u64,
    outcome: Delivery,
    delivered: &[u32],
    wall: i64,
) -> (Delivery, Option<String>) {
    let Delivery::Deferred(err) = outcome else {
        *held = None;
        return (outcome, None);
    };
    let h = match held {
        Some(h) if h.sms_id == sms_id => h,
        _ => held.insert(HeldSms {
            sms_id,
            first_deferred_at: wall,
            ..Default::default()
        }),
    };
    let mut rules = delivered.to_vec();
    rules.sort_unstable();
    rules.dedup();
    h.delivered_rules = rules;
    if h.online_secs < HOLD_TIMEOUT_SECS {
        return (Delivery::Deferred(err), None);
    }
    let alert = format!(
        "SMS {sms_id} not forwarded after {}h online, skipped: {err}",
        h.online_secs / 3600
    );
    *held = None;
    (
        Delivery::Skipped(format!("{HOLD_TIMEOUT_NOTE}: {err}")),
        Some(alert),
    )
}

/// What one complete read of the device's SMS list gave.
#[derive(Debug, Clone, Default)]
pub struct DeviceSnapshot {
    /// Highest id seen in any row (received or not).
    pub max_id: u64,
    /// Received messages: (id, time key).
    pub received: Vec<(u64, Option<i64>)>,
}

#[derive(Debug, PartialEq)]
pub struct ForwardPlan {
    pub forward_ids: Vec<u64>,
    pub new_watermark: u64,
    pub new_last_time: Option<i64>,
    pub rollback: bool,
}

pub fn max_time(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// Decide what to forward (the agent's, unchanged; see its doc).
pub fn plan_forward(
    watermark: u64,
    last_time: Option<i64>,
    snapshot: Result<&DeviceSnapshot, &str>,
) -> ForwardPlan {
    let unchanged = ForwardPlan {
        forward_ids: Vec::new(),
        new_watermark: watermark,
        new_last_time: last_time,
        rollback: false,
    };
    let snap = match snapshot {
        Ok(s) => s,
        Err(_) => return unchanged,
    };
    if snap.max_id > 0 && snap.max_id < watermark {
        let mut forward: Vec<(u64, Option<i64>)> = match last_time {
            Some(lt) => snap
                .received
                .iter()
                .copied()
                .filter(|&(_, t)| matches!(t, Some(t) if t > lt))
                .collect(),
            None => Vec::new(),
        };
        forward.sort_by_key(|&(id, _)| id);
        let new_last_time = match last_time {
            Some(_) => forward
                .iter()
                .fold(last_time, |acc, &(_, t)| max_time(acc, t)),
            None => snap
                .received
                .iter()
                .fold(None, |acc, &(_, t)| max_time(acc, t)),
        };
        return ForwardPlan {
            forward_ids: forward.into_iter().map(|(id, _)| id).collect(),
            new_watermark: snap.max_id,
            new_last_time,
            rollback: true,
        };
    }
    let mut forward: Vec<(u64, Option<i64>)> = snap
        .received
        .iter()
        .copied()
        .filter(|&(id, _)| id > watermark)
        .collect();
    forward.sort_by_key(|&(id, _)| id);
    let mut new_last_time = last_time;
    if new_last_time.is_none() {
        new_last_time = snap
            .received
            .iter()
            .filter(|&&(id, _)| id <= watermark)
            .fold(None, |acc, &(_, t)| max_time(acc, t));
    }
    let new_last_time = forward
        .iter()
        .fold(new_last_time, |acc, &(_, t)| max_time(acc, t));
    ForwardPlan {
        new_watermark: forward.last().map(|&(id, _)| id).unwrap_or(watermark),
        forward_ids: forward.into_iter().map(|(id, _)| id).collect(),
        new_last_time,
        rollback: false,
    }
}

/// A received SMS with a fresh id but an old date (the firmware renumbers the SIM's stored SMS
/// when it reads a card again): skipped, the watermark still moves past it.
pub fn is_reimported(time: Option<i64>, last_time: Option<i64>) -> bool {
    matches!((time, last_time), (Some(t), Some(lt)) if t < lt - REIMPORT_MARGIN_SECS)
}

/// Page through the device's SMS above the watermark and forward them (the agent's `run_pass`,
/// over [`Item`]s; see its doc). Returns how many SMS were handled.
pub fn run_pass(
    watermark: u64,
    last_time: Option<i64>,
    max_id: u64,
    fetch: &mut dyn FnMut(u64, u64) -> Result<Page, String>,
    forward: &mut dyn FnMut(&Item) -> Delivery,
    commit: &mut dyn FnMut(u64, Option<i64>),
) -> Result<usize, String> {
    if max_id == 0 || max_id == watermark {
        return Ok(0);
    }
    let mut sent = 0;
    if max_id < watermark {
        let mut after = 0;
        let mut all: Vec<Item> = Vec::new();
        loop {
            let page = fetch(after, LIST_PAGE)?;
            let last = page.items.iter().map(|i| i.id).max().unwrap_or(after);
            if page.has_more && last <= after {
                return Err(format!("sms.list_after made no progress after id {after}"));
            }
            after = last;
            all.extend(page.items);
            if !page.has_more {
                break;
            }
        }
        let mut snap = DeviceSnapshot {
            max_id,
            received: Vec::new(),
        };
        let mut decoded: Vec<Item> = Vec::new();
        for item in &all {
            snap.max_id = snap.max_id.max(item.id);
            if let Some(t) = item.received_time() {
                snap.received.push((item.id, t));
                decoded.push(*item);
            }
        }
        let plan = plan_forward(watermark, last_time, Ok(&snap));
        let mut lt = last_time;
        for id in &plan.forward_ids {
            if let Some(sms) = decoded.iter().find(|m| m.id == *id) {
                if let Delivery::Deferred(_) = forward(sms) {
                    return Ok(sent);
                }
                sent += 1;
                lt = max_time(lt, sms.time);
                commit(watermark, lt);
            }
        }
        commit(plan.new_watermark, plan.new_last_time);
        return Ok(sent);
    }
    let (mut wm, mut lt) = (watermark, last_time);
    loop {
        let page = fetch(wm, LIST_PAGE)?;
        let mut progressed = false;
        for item in &page.items {
            if item.id <= wm {
                continue;
            }
            progressed = true;
            if let Some(time) = item.received_time().filter(|t| !is_reimported(*t, lt)) {
                if let Delivery::Deferred(_) = forward(item) {
                    return Ok(sent);
                }
                sent += 1;
                lt = max_time(lt, time);
            }
            wm = item.id;
            commit(wm, lt);
        }
        if !page.has_more {
            return Ok(sent);
        }
        if !progressed {
            return Err(format!("sms.list_after made no progress after id {wm}"));
        }
    }
}

/* -------------------------------------------------------------- *
 *  Replay: one recorded pass / delivery through this copy
 * -------------------------------------------------------------- */

/// What a recorded page fetch got.
#[derive(Clone, Debug, PartialEq)]
pub enum PageAnswer {
    Ok(Page),
    Err(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordedPage {
    pub after: u64,
    pub limit: u64,
    pub answer: PageAnswer,
}

/// A pass as the agent recorded it: where it started and what each call got.
#[derive(Clone, Debug, PartialEq)]
pub struct PassIn {
    pub watermark: u64,
    pub last_time: Option<i64>,
    pub max_id: u64,
    pub pages: Vec<RecordedPage>,
    /// (SMS id, outcome) in order; the outcomes' texts are not recorded
    pub forwards: Vec<(u64, Delivery)>,
}

/// What this copy did with the recorded answers.
#[derive(Clone, Debug, PartialEq)]
pub struct PassOut {
    /// (after, limit) of every page asked for
    pub asked: Vec<(u64, u64)>,
    /// ids forwarded, in order
    pub forwarded: Vec<u64>,
    pub commits: Vec<(u64, Option<i64>)>,
    pub result: Result<usize, String>,
}

/// Run [`run_pass`] answering the n-th page fetch with the n-th recorded page and the n-th forward
/// with the n-th recorded outcome, whatever was asked (the host compares what was asked). Past the
/// recording: a fetch fails, a forward is deferred (both end the pass).
pub fn replay_pass(input: &PassIn) -> PassOut {
    let (mut asked, mut forwarded, mut commits) = (Vec::new(), Vec::new(), Vec::new());
    let result = run_pass(
        input.watermark,
        input.last_time,
        input.max_id,
        &mut |after, limit| {
            let n = asked.len();
            asked.push((after, limit));
            match input.pages.get(n).map(|p| &p.answer) {
                Some(PageAnswer::Ok(p)) => Ok(p.clone()),
                Some(PageAnswer::Err(e)) => Err(e.clone()),
                None => Err("no recorded page".to_string()),
            }
        },
        &mut |item| {
            let n = forwarded.len();
            forwarded.push(item.id);
            match input.forwards.get(n) {
                Some((_, d)) => d.clone(),
                None => Delivery::Deferred(String::new()),
            }
        },
        &mut |w, t| commits.push((w, t)),
    );
    PassOut {
        asked,
        forwarded,
        commits,
        result,
    }
}

/// A delivery's rule results (true = transient failure, false = permanent) through
/// [`delivery_outcome`] and [`settle_hold`]: the outcome, the hold after, whether it alerted.
pub fn settle(
    held: Option<HeldSms>,
    sms_id: u64,
    failures: &[bool],
    delivered: &[u32],
    wall: i64,
) -> (Delivery, Option<HeldSms>, bool) {
    let errors: Vec<String> = failures
        .iter()
        .map(|t| {
            if *t {
                "transient".to_string()
            } else {
                "x (permanent error)".to_string()
            }
        })
        .collect();
    let mut held = held;
    let (o, a) = settle_hold(
        &mut held,
        sms_id,
        delivery_outcome(&errors),
        delivered,
        wall,
    );
    (o, held, a.is_some())
}

#[cfg(test)]
mod tests;
