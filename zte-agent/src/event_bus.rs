use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;

use serde_json::Value;

const RESTART_DELAY_SECS: u64 = 5;
const IGNORED_EVENTS: &[&str] = &["zwrt_deviceui_event.touchstatus"];
/// Events queued per subscriber before new ones are dropped. The only reader
/// (sms_forward's connectivity monitor) drains them as they come, so this
/// fills only if that reader is stuck; unbounded, a stuck reader would grow
/// the queue for ever. What a full queue drops is the newest event, so the
/// reader's view may lag until the next one after it recovers.
const QUEUE: usize = 64;

struct Subscriber {
    event: String,
    tx: mpsc::SyncSender<Value>,
    dropped: AtomicU64,
}

pub struct EventBus {
    subscribers: Mutex<Vec<Subscriber>>,
}

impl EventBus {
    pub fn new() -> Self {
        EventBus {
            subscribers: Mutex::new(Vec::new()),
        }
    }

    /// Register interest in a specific ubus event (exact name). Returns a
    /// receiver that will get the event payload each time it fires, up to
    /// [`QUEUE`] unread ones; past that, new ones are dropped and counted.
    pub fn subscribe(&self, event_name: &str) -> mpsc::Receiver<Value> {
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let mut subs = self.subscribers.lock().unwrap();
        subs.push(Subscriber {
            event: event_name.to_string(),
            tx,
            dropped: AtomicU64::new(0),
        });
        rx
    }

    /// `ubus listen` arguments: only the subscribed events. With none it
    /// would print every event on the bus (the touch screen's touch events
    /// among them) for us to parse and throw away. Dispatch matches names
    /// exactly, so nothing a subscriber could receive is lost by this.
    fn listen_args(&self) -> Vec<String> {
        let subs = self.subscribers.lock().unwrap();
        let mut names: Vec<String> = Vec::new();
        for s in subs.iter() {
            if !names.contains(&s.event) {
                names.push(s.event.clone());
            }
        }
        names
    }

    /// Spawn the listener thread. Consumes self into an Arc-compatible form.
    /// Call this after all subscriptions are registered.
    pub fn start(self) {
        std::thread::spawn(move || self.run_loop());
    }

    fn run_loop(&self) {
        let events = self.listen_args();
        if events.is_empty() {
            // Bare `ubus listen` means everything; nobody would read it.
            eprintln!("[event_bus] no subscribers, not listening");
            return;
        }
        loop {
            match spawn_ubus_listen(&events) {
                Ok(mut child) => {
                    eprintln!("[event_bus] ubus listen started (pid {})", child.id());
                    if let Some(stdout) = child.stdout.take() {
                        self.read_events(stdout);
                    }
                    // Process exited — reap it
                    let _ = child.wait();
                    eprintln!("[event_bus] ubus listen exited, restarting in {RESTART_DELAY_SECS}s");
                }
                Err(e) => {
                    eprintln!("[event_bus] failed to spawn ubus listen: {e}");
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(RESTART_DELAY_SECS));
        }
    }

    fn read_events(&self, stdout: std::process::ChildStdout) {
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break, // pipe closed
            };

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // ubus listen outputs: { "event_name": { ...payload... } }
            let parsed: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let obj = match parsed.as_object() {
                Some(o) => o,
                None => continue,
            };

            // Extract single top-level key as event name
            for (event_name, payload) in obj {
                if IGNORED_EVENTS.contains(&event_name.as_str()) {
                    continue;
                }

                self.dispatch(event_name, payload);
            }
        }
    }

    fn dispatch(&self, event_name: &str, payload: &Value) {
        let subs = self.subscribers.lock().unwrap();
        for sub in subs.iter() {
            if sub.event == event_name {
                // Never blocks the reader of `ubus listen`: a full queue drops
                // this event; a gone receiver is not a drop worth counting.
                if let Err(mpsc::TrySendError::Full(_)) = sub.tx.try_send(payload.clone()) {
                    let n = sub.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                    if n.is_power_of_two() {
                        eprintln!("[event_bus] {event_name}: queue full, {n} event(s) dropped so far");
                    }
                }
            }
        }
    }
}

fn spawn_ubus_listen(events: &[String]) -> Result<Child, String> {
    Command::new("ubus")
        .arg("listen")
        .args(events)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn ubus listen: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn listens_only_to_subscribed_events() {
        let bus = EventBus::new();
        assert!(bus.listen_args().is_empty());
        let _a = bus.subscribe("zwrt_servicestatus");
        let _b = bus.subscribe("router_event_wan_connect_status");
        let _c = bus.subscribe("zwrt_servicestatus");
        assert_eq!(bus.listen_args(), ["zwrt_servicestatus", "router_event_wan_connect_status"]);
    }

    #[test]
    fn a_full_queue_drops_and_counts_instead_of_growing() {
        let bus = EventBus::new();
        let rx = bus.subscribe("zwrt_servicestatus");
        let other = bus.subscribe("router_event_wan_connect_status");
        for i in 0..QUEUE + 10 {
            bus.dispatch("zwrt_servicestatus", &json!({"n": i}));
        }
        let dropped = |i: usize| bus.subscribers.lock().unwrap()[i].dropped.load(Ordering::Relaxed);
        assert_eq!(dropped(0), 10);
        assert_eq!(dropped(1), 0, "other events are not touched");
        assert!(other.try_recv().is_err());
        // The oldest are kept; once read, there is room again.
        assert_eq!(rx.try_recv().unwrap()["n"], 0);
        bus.dispatch("zwrt_servicestatus", &json!({"n": "new"}));
        assert_eq!(dropped(0), 10);
        assert_eq!(rx.try_iter().count(), QUEUE);
    }

    #[test]
    fn a_gone_receiver_is_not_a_drop() {
        let bus = EventBus::new();
        drop(bus.subscribe("zwrt_servicestatus"));
        for _ in 0..QUEUE + 5 {
            bus.dispatch("zwrt_servicestatus", &json!({}));
        }
        assert_eq!(bus.subscribers.lock().unwrap()[0].dropped.load(Ordering::Relaxed), 0);
    }
}
