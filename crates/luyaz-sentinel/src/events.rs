//! The sentinel's events (u60-sentinel.md §6, D21, C-R9): every alert the sentinel raises — a
//! hand-back to the vendor UI, a crash of our UI, a Wi-Fi takeover — gets an id (the boot id's
//! first 8 hex digits, `-`, a sequence number kept in `/data/luyaz/screen/events.seq` (Phase 10 R4; was `/data/u60-uid/`) across sentinel
//! restarts), is written to `/data/luyaz/screen/events` (synced) before anything else, and then:
//!
//! 1. POSTed to the core (`/v2/sentinel/event`), which queues it as an alert once per id;
//! 2. no answer within 5 s → the sentinel queues it itself (`u60-alerts` `add_once`: if the core
//!    got there first, or gets there later, the id is already in the queue and nothing doubles);
//! 3. delivered either way → a `D` line in the events file. A sentinel that dies in between finds
//!    the event without its `D` at start and delivers it again (same id, so still once).
//!
//! While the core itself is being replaced (a datad ship transaction with a live executor, D6),
//! delivery waits: the core is expected back within the trial, and its replacement takes the POST.
//! Whether the alert then goes out as an SMS is the send states' business (`u60-alerts::sms`).
//!
//! Events file lines: `E \t id \t wall \t uptime \t kind \t text` and `D \t id`; trimmed to the
//! last [`KEEP`] lines (undelivered `E` lines are kept whatever their age).

use std::collections::VecDeque;

pub const SEQ_FILE: &str = "/data/luyaz/screen/events.seq";
pub const EVENTS_FILE: &str = "/data/luyaz/screen/events";
pub const KEEP: usize = 200;

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub id: String,
    pub wall: i64,
    pub uptime: u64,
    pub kind: String,
    pub text: String,
}

impl Event {
    pub fn line(&self) -> String {
        format!(
            "E\t{}\t{}\t{}\t{}\t{}",
            self.id,
            self.wall,
            self.uptime,
            self.kind,
            self.text.replace(['\t', '\n'], " ")
        )
    }
}

/// How a delivery went.
#[derive(Debug, PartialEq)]
pub enum Post {
    /// The core queued it (or had it already).
    Acked,
    /// No answer in time, refused, or an error.
    NoAck(String),
}

pub trait EventIo {
    fn wall(&mut self) -> i64;
    fn uptime(&mut self) -> u64;
    /// The boot id's first 8 hex digits.
    fn boot8(&mut self) -> String;
    fn read(&mut self, path: &str) -> Option<String>;
    /// Whole-file write, synced.
    fn write_synced(&mut self, path: &str, content: &str);
    /// One line appended, synced.
    fn append_synced(&mut self, path: &str, line: &str);
    /// POST to the core, at most 5 s.
    fn post(&mut self, ev: &Event) -> Post;
    /// `add_once` into the alert queue; false when even that failed.
    fn enqueue(&mut self, ev: &Event) -> bool;
    /// A datad (core) ship transaction with a live executor.
    fn core_being_replaced(&mut self) -> bool;
    fn log(&mut self, msg: &str);
}

#[derive(Default)]
pub struct Events {
    pending: VecDeque<Event>,
    loaded: bool,
}

/// The events file cut to its last [`KEEP`] lines plus every undelivered `E` line; None when it
/// is short enough already.
pub fn trimmed(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= KEEP {
        return None;
    }
    let keep_ids: Vec<String> = undelivered(text).into_iter().map(|e| e.id).collect();
    let cut = lines.len() - KEEP;
    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, l)| {
            *i >= cut
                || keep_ids
                    .iter()
                    .any(|id| l.starts_with(&format!("E\t{id}\t")))
        })
        .map(|(_, l)| *l)
        .collect();
    Some(kept.join("\n") + "\n")
}

/// The `E` lines without a `D` line, oldest first.
pub fn undelivered(text: &str) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::new();
    for l in text.lines() {
        let f: Vec<&str> = l.splitn(6, '\t').collect();
        match f.as_slice() {
            ["E", id, wall, up, kind, text] => {
                if let (Ok(wall), Ok(uptime)) = (wall.parse(), up.parse()) {
                    out.push(Event {
                        id: id.to_string(),
                        wall,
                        uptime,
                        kind: kind.to_string(),
                        text: text.to_string(),
                    });
                }
            }
            ["D", id] => out.retain(|e| e.id != *id),
            _ => {}
        }
    }
    out
}

impl Events {
    /// The events a previous run left undelivered, once (call before the first [`Events::raise`]).
    pub fn load(&mut self, io: &mut dyn EventIo) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let left = undelivered(&io.read(EVENTS_FILE).unwrap_or_default());
        if !left.is_empty() {
            io.log(&format!(
                "events: {} left undelivered by the last run; delivering",
                left.len()
            ));
        }
        self.pending.extend(left);
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// A new event: id, recorded, queued for delivery.
    pub fn raise(&mut self, io: &mut dyn EventIo, kind: &str, text: &str) -> Event {
        self.load(io);
        let seq = io
            .read(SEQ_FILE)
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0)
            + 1;
        io.write_synced(SEQ_FILE, &format!("{seq}\n"));
        let ev = Event {
            id: format!("{}-{seq}", io.boot8()),
            wall: io.wall(),
            uptime: io.uptime(),
            kind: kind.to_string(),
            text: text.to_string(),
        };
        io.append_synced(EVENTS_FILE, &ev.line());
        self.pending.push_back(ev.clone());
        ev
    }

    /// Deliver what is pending, oldest first; stops (to try again later) while the core is being
    /// replaced, or when even queuing it ourselves failed.
    pub fn deliver(&mut self, io: &mut dyn EventIo) {
        self.load(io);
        let mut done = false;
        while let Some(ev) = self.pending.front().cloned() {
            if io.core_being_replaced() {
                return;
            }
            match io.post(&ev) {
                Post::Acked => {}
                Post::NoAck(why) => {
                    if !io.enqueue(&ev) {
                        io.log(&format!(
                            "events: {} not queued (core: {why}); retrying",
                            ev.id
                        ));
                        break;
                    }
                    io.log(&format!("events: {} queued here (core: {why})", ev.id));
                }
            }
            io.append_synced(EVENTS_FILE, &format!("D\t{}", ev.id));
            self.pending.pop_front();
            done = true;
        }
        if done {
            self.trim(io);
        }
    }

    fn trim(&mut self, io: &mut dyn EventIo) {
        if let Some(t) = io.read(EVENTS_FILE).as_deref().and_then(trimmed) {
            io.write_synced(EVENTS_FILE, &t);
        }
    }
}

#[cfg(test)]
mod tests;
