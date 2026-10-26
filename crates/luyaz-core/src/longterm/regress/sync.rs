//! The alert queue read back (CEO review G3, OV-C2; eng review round 3 D9): every check the
//! alerts after `ingested_seq` are taken in order, once each (by seq, not by id):
//!
//! - a regress transition (`[rg-…]`) is kept with its incident; the sentinel's kinds
//!   (`regress-recorder`, `regress-core-down`, `regress-disk`) open and close their incidents here;
//! - a point event (a crash, the touch UI giving up, the sentinel silent) joins the open incident
//!   of its kind when it came within the window of the last one, else opens one (`q<seq>`).
//!
//! A queue trimmed past the read point is a gap (`gap_at`, `/v2/incidents` `ingest_gap`):
//! nothing is made up for what was lost.

use luyaz_api::alerts::{Event, text_body, text_id};

use super::{At, Incident, Regress, SENTINEL_KINDS, clip, is_point, split_tid};

/// Wall seconds below this are "clock not set" (luyaz-alerts' rule).
const CLOCK_SANE_AFTER: i64 = 1_704_067_200;

/// When an alert was queued, as far as the queue tells: its wall clock and uptime; the current
/// boot's when its uptime and wall clock fit this boot.
pub fn event_at(e: &Event, now: &At) -> At {
    let ok = e.wall >= CLOCK_SANE_AFTER;
    let fits = e.uptime as i64 <= now.up
        && (!ok || !now.ok || ((now.wall - e.wall) - (now.up - e.uptime as i64)).abs() < 300);
    At {
        boot: if fits {
            now.boot.clone()
        } else {
            String::new()
        },
        up: e.uptime as i64,
        wall: e.wall,
        ok,
    }
}

impl Regress {
    /// The queue as it is now (oldest first).
    pub fn ingest(&mut self, events: &[Event], now: &At) {
        let from = self.file.ingested_seq;
        if let Some(first) = events.first()
            && from > 0
            && first.seq > from + 1
        {
            self.file.gap_at = Some(now.clone());
            self.log.push(format!(
                "[regress] gap: alerts {}..{} were trimmed before they were read",
                from + 1,
                first.seq - 1
            ));
            self.dirty = true;
        }
        let newest = events.last().map_or(from, |e| e.seq);
        if newest < from {
            // the alert files were reset: read from their start
            self.file.ingested_seq = 0;
        }
        let start = self.file.ingested_seq;
        for e in events.iter().filter(|e| e.seq > start) {
            self.one(e, now);
            self.file.ingested_seq = e.seq;
            self.dirty = true;
        }
    }

    fn one(&mut self, e: &Event, now: &At) {
        let at = event_at(e, now);
        if let Some(tid) = text_id(&e.text).filter(|t| t.starts_with("rg-")) {
            let Some((iid, what)) = split_tid(tid) else {
                return;
            };
            let iid = iid.to_string();
            let tid = tid.to_string();
            let sentinel = SENTINEL_KINDS.contains(&e.kind.as_str());
            let known = self
                .file
                .open
                .iter()
                .chain(&self.file.closed)
                .any(|i| i.id == iid);
            if !known && sentinel && what == "open" {
                let mut i = Incident::new(iid.clone(), &e.kind, at.clone());
                i.detail = clip(text_body(&e.text));
                self.log
                    .push(format!("[regress] open {iid} {} (sentinel)", e.kind));
                self.fresh.push(iid.clone());
                self.file.open.push(i);
            }
            for i in self.file.open.iter_mut().chain(self.file.closed.iter_mut()) {
                if i.id == iid {
                    i.keep_alert(e.seq);
                    i.pending.retain(|p| p.id != tid);
                }
            }
            if sentinel
                && what == "close"
                && let Some(k) = self.file.open.iter().position(|i| i.id == iid)
            {
                let mut i = self.file.open.remove(k);
                i.to = Some(at);
                i.end = Some("recovered".into());
                self.log.push(format!(
                    "[regress] close {iid} {} recovered (sentinel)",
                    e.kind
                ));
                self.ended.push(i.id.clone());
                self.file.closed.push(i);
            }
            return;
        }
        // Wi-Fi the watchdog could not bring back: open until it does (design §3)
        if e.kind == "wifi-restore-failed" || e.kind == "wifi-takeover" {
            let open = self
                .file
                .open
                .iter()
                .position(|i| i.kind == "wifi-restore-failed");
            match (e.kind.as_str(), open) {
                ("wifi-restore-failed", Some(k)) => {
                    self.file.open[k].keep_alert(e.seq);
                }
                ("wifi-restore-failed", None) => {
                    let id = format!("q{}", e.seq);
                    let mut i = Incident::new(id.clone(), &e.kind, at);
                    i.build = self.build.clone();
                    i.detail = clip(text_body(&e.text));
                    i.keep_alert(e.seq);
                    self.log.push(format!("[regress] open {id} {}", e.kind));
                    self.fresh.push(id);
                    self.file.open.push(i);
                }
                ("wifi-takeover", Some(k)) => {
                    let mut i = self.file.open.remove(k);
                    i.keep_alert(e.seq);
                    i.to = Some(at);
                    i.end = Some("recovered".into());
                    self.log.push(format!(
                        "[regress] close {} wifi-restore-failed recovered",
                        i.id
                    ));
                    self.ended.push(i.id.clone());
                    self.file.closed.push(i);
                }
                _ => {}
            }
            return;
        }
        if !luyaz_api::alerts::is_point(&e.kind) {
            return;
        }
        let near = self.file.open.iter().position(|i| {
            i.kind == e.kind
                && i.last
                    .as_ref()
                    .and_then(|l| l.until(&at))
                    .is_some_and(|s| (0..=super::POINT_WINDOW_S).contains(&s))
        });
        match near {
            Some(k) => {
                let i = &mut self.file.open[k];
                i.count += 1;
                i.last = Some(at);
                i.keep_alert(e.seq);
                self.log
                    .push(format!("[regress] again {} {} #{}", i.id, i.kind, i.count));
            }
            None => {
                let id = format!("q{}", e.seq);
                let mut i = Incident::new(id.clone(), &e.kind, at.clone());
                i.last = Some(at);
                i.build = self.build.clone();
                i.detail = clip(text_body(&e.text));
                i.keep_alert(e.seq);
                self.log.push(format!("[regress] open {id} {}", e.kind));
                self.fresh.push(id);
                self.file.open.push(i);
            }
        }
    }

    /// The owner accepts a relative incident's level (design §4; CEO R3-4): open for 3 days at
    /// least, and enough recent hours.
    pub fn accept<'a>(
        &mut self,
        kind: &str,
        lines: impl Iterator<Item = &'a serde_json::Value>,
        now: &At,
    ) -> Result<String, String> {
        if !matches!(kind, "regress-mem" | "regress-power") {
            return Err(format!(
                "only regress-mem and regress-power can be accepted, not {kind}"
            ));
        }
        let Some(k) = self.file.open.iter().position(|i| i.kind == kind) else {
            return Err(format!("no open {kind} incident"));
        };
        let age = self.file.open[k].from.until(now);
        if age.is_none_or(|s| s < 3 * 86_400) {
            return Err(format!(
                "{kind} has been open {} h; accept needs 3 days",
                age.map_or("?".into(), |s| (s / 3600).to_string())
            ));
        }
        if !now.ok {
            return Err("the clock is not trusted yet".into());
        }
        let cell = self.file.open[k].sub.clone();
        let said = self.base.accept(kind, cell.as_deref(), lines, now.wall)?;
        self.close(
            k,
            now,
            "recovered",
            "accepted as the new normal by the owner",
        );
        self.log.push(format!("[regress] accept {kind}: {said}"));
        Ok(said)
    }
}

/// Point incidents whose window passed end `quiet` (eng review round 3 D8): a reboot row's
/// after 6 h, the others after an hour.
pub fn expire(r: &mut Regress, now: &At) {
    let mut k = 0;
    while k < r.file.open.len() {
        let i = &r.file.open[k];
        if !is_point(&i.kind) {
            k += 1;
            continue;
        }
        let window = if i.kind == "regress-reboot" {
            super::REBOOT_WINDOW_S
        } else {
            super::POINT_WINDOW_S
        };
        let last = i.last.as_ref().unwrap_or(&i.from);
        let gone = match last.until(now) {
            Some(s) => s > window,
            // another boot and no clock to tell by: a crash row ends; a reboot row waits for one
            None => !last.ok && i.kind != "regress-reboot",
        };
        if gone {
            let what = if window > 3600 { "6 h" } else { "1 h" };
            r.close(k, now, "quiet", &format!("none for {what}"));
        } else {
            k += 1;
        }
    }
}
