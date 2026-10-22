//! The four event timings (D15) against the real alert queue in a temp dir.

use std::collections::HashMap;

use super::*;
use luyaz_alerts::Alerts;

struct Fake {
    files: HashMap<String, String>,
    alerts: Alerts,
    /// What the core does with a POST: queue it and ack, or not answer.
    core_up: bool,
    /// The core queues it but its answer comes after 5 s (counts as no ack).
    core_late: bool,
    replacing: bool,
    boot: String,
    posts: usize,
    logs: Vec<String>,
}

impl Fake {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("luyaz-events-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        Fake {
            files: HashMap::new(),
            alerts: Alerts::at(&d),
            core_up: true,
            core_late: false,
            replacing: false,
            boot: "8559f5dc".into(),
            posts: 0,
            logs: vec![],
        }
    }
    fn queue(&self) -> Vec<String> {
        self.alerts.events().into_iter().map(|e| e.text).collect()
    }
}

impl EventIo for Fake {
    fn wall(&mut self) -> i64 {
        1_791_500_000
    }
    fn uptime(&mut self) -> u64 {
        100
    }
    fn boot8(&mut self) -> String {
        self.boot.clone()
    }
    fn read(&mut self, path: &str) -> Option<String> {
        self.files.get(path).cloned()
    }
    fn write_synced(&mut self, path: &str, content: &str) {
        self.files.insert(path.into(), content.into());
    }
    fn append_synced(&mut self, path: &str, line: &str) {
        self.files
            .entry(path.into())
            .or_default()
            .push_str(&format!("{line}\n"));
    }
    fn post(&mut self, ev: &Event) -> Post {
        self.posts += 1;
        if !self.core_up {
            return Post::NoAck("refused".into());
        }
        self.alerts
            .add_once(&ev.id, &ev.kind, &ev.text, ev.wall, ev.uptime)
            .unwrap();
        if self.core_late {
            return Post::NoAck("timed out".into());
        }
        Post::Acked
    }
    fn enqueue(&mut self, ev: &Event) -> bool {
        self.alerts
            .add_once(&ev.id, &ev.kind, &ev.text, ev.wall, ev.uptime)
            .is_ok()
    }
    fn core_being_replaced(&mut self) -> bool {
        self.replacing
    }
    fn log(&mut self, msg: &str) {
        self.logs.push(msg.into());
    }
}

#[test]
fn core_acks_one_alert_ids_count_on() {
    let mut io = Fake::new("ack");
    let mut e = Events::default();
    let ev = e.raise(&mut io, "ui-gave-up", "gave up after 2");
    assert_eq!(ev.id, "8559f5dc-1");
    e.deliver(&mut io);
    let ev2 = e.raise(&mut io, "ui-crash", "x");
    e.deliver(&mut io);
    assert_eq!(ev2.id, "8559f5dc-2");
    assert_eq!(
        io.queue(),
        vec!["[8559f5dc-1] gave up after 2", "[8559f5dc-2] x"]
    );
    assert!(undelivered(&io.files[EVENTS_FILE]).is_empty());
}

#[test]
fn post_without_answer_the_sentinel_queues_it() {
    let mut io = Fake::new("noack");
    io.core_up = false;
    let mut e = Events::default();
    e.raise(&mut io, "ui-gave-up", "g");
    e.deliver(&mut io);
    assert_eq!(io.queue(), vec!["[8559f5dc-1] g"]);
    assert!(io.logs.iter().any(|l| l.contains("queued here")));
}

#[test]
fn core_answers_after_5_s_still_once() {
    let mut io = Fake::new("late");
    io.core_late = true;
    let mut e = Events::default();
    e.raise(&mut io, "ui-gave-up", "g");
    e.deliver(&mut io);
    assert_eq!(io.queue().len(), 1);
}

#[test]
fn sentinel_dies_before_delivering_the_next_run_delivers_once() {
    let mut io = Fake::new("restart");
    let mut e = Events::default();
    // recorded, then the process died before the POST
    e.raise(&mut io, "ui-gave-up", "g");
    drop(e);
    // and a POST that got through but died before writing D: queued already
    let mut e = Events::default();
    e.load(&mut io);
    assert_eq!(e.pending(), 1);
    io.alerts
        .add_once("8559f5dc-1", "ui-gave-up", "g", 1, 1)
        .unwrap();
    e.deliver(&mut io);
    assert_eq!(io.queue(), vec!["[8559f5dc-1] g"]);
    // the sequence goes on from the file across restarts
    let ev = Events::default().raise(&mut io, "ui-crash", "c");
    assert_eq!(ev.id, "8559f5dc-2");
}

#[test]
fn waits_while_the_core_is_replaced() {
    let mut io = Fake::new("yield");
    io.replacing = true;
    let mut e = Events::default();
    e.raise(&mut io, "ui-gave-up", "g");
    e.deliver(&mut io);
    assert_eq!((io.posts, e.pending()), (0, 1));
    assert!(io.queue().is_empty());
    io.replacing = false;
    e.deliver(&mut io);
    assert_eq!((io.posts, e.pending(), io.queue().len()), (1, 0, 1));
}

#[test]
fn the_file_is_trimmed_but_undelivered_stays() {
    let mut lines = String::from("E\t8559f5dc-1\t1\t1\tk\told undelivered\n");
    for i in 2..400 {
        lines.push_str(&format!("E\t8559f5dc-{i}\t1\t1\tk\tt\nD\t8559f5dc-{i}\n"));
    }
    let t = trimmed(&lines).unwrap();
    assert_eq!(t.lines().count(), KEEP + 1);
    assert!(t.starts_with("E\t8559f5dc-1\t"));
    assert_eq!(undelivered(&t).len(), 1);
    assert_eq!(trimmed(&t[..t.find("E\t8559f5dc-3").unwrap()]), None);
}
