//! `GET /v2/timeline/today`: the home card's 「今天」 timeline, read from the long-term layer's two
//! files and the incidents (no store of its own). Since local midnight: the device clock is local
//! time labelled UTC, so midnight is the wall second rounded down to a whole day. Without a
//! trusted clock there is no "today": `clock_trusted: false` and nothing listed.
//!
//! Both files are read a line at a time (events ≤ [`super::EVENTS_CAP`], hours a few hundred KB);
//! at most [`EVENTS_MAX`] events (the newest) and one point per hour.
//!
//! Event kinds (`kind`): `scenario` (a switch by detection), `link_down` / `link_up` (the cellular
//! link), `boot` (with the firmware's reason `code` when known), `incident_open` /
//! `incident_close`, `charge_start` / `charge_stop`, `repair` (an automatic change or revert).
//! Each: `{t, kind, text_zh, text_en, tone}` (`tone`: ok / warn / bad / neutral), oldest first.

use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::BufRead;
use std::path::{Path, PathBuf};

pub const EVENTS_MAX: usize = 200;
const HOURS_MAX: usize = 25;
const DAY_S: i64 = 86_400;

fn ev(t: i64, kind: &str, zh: String, en: String, tone: &str) -> Value {
    json!({"t": t, "kind": kind, "text_zh": zh, "text_en": en, "tone": tone})
}

/// The timeline item for one `events.jsonl` line, `None` for kinds the timeline leaves out.
fn item(v: &Value, t: i64) -> Option<Value> {
    let s = |k: &str| v[k].as_str().unwrap_or("");
    Some(match s("k") {
        "scenario" => {
            let name = v["name"]
                .as_str()
                .filter(|n| !n.is_empty())
                .unwrap_or(s("to"));
            if v["ok"].as_bool() == Some(false) {
                ev(
                    t,
                    "scenario",
                    format!("切到「{name}」没成功，回到外出"),
                    format!("Switching to {name} failed; back to away"),
                    "warn",
                )
            } else {
                ev(
                    t,
                    "scenario",
                    format!("情景：{name}"),
                    format!("Scenario: {name}"),
                    "neutral",
                )
            }
        }
        "link" if s("state") == "down" => ev(
            t,
            "link_down",
            "蜂窝断开".into(),
            "Cellular link down".into(),
            "bad",
        ),
        "link" if s("state") == "up" => ev(
            t,
            "link_up",
            "蜂窝恢复".into(),
            "Cellular link back".into(),
            "ok",
        ),
        "boot" => {
            let code = v["code"].as_i64();
            let mut e = match code {
                Some(c) => ev(
                    t,
                    "boot",
                    format!("开机（原因码 {c}）"),
                    format!("Booted (reason {c})"),
                    "neutral",
                ),
                None => ev(t, "boot", "开机".into(), "Booted".into(), "neutral"),
            };
            e["code"] = json!(code);
            e["boot"] = v["boot"].clone();
            e
        }
        "charge" if s("state") == "start" => ev(
            t,
            "charge_start",
            "开始充电".into(),
            "Charging".into(),
            "ok",
        ),
        "charge" if s("state") == "stop" => ev(
            t,
            "charge_stop",
            "停止充电".into(),
            "Charging stopped".into(),
            "neutral",
        ),
        "repair" => {
            let what = s("item");
            let (zh, en, tone) = match s("phase") {
                "rolled_back" => (
                    format!("自动退回：{what}"),
                    format!("Reverted: {what}"),
                    "warn",
                ),
                "rollback_failed" => (
                    format!("自动退回没成功：{what}"),
                    format!("Revert failed: {what}"),
                    "bad",
                ),
                "confirmed" | "unverified" => (
                    format!("自动修复：{what}"),
                    format!("Repaired: {what}"),
                    "ok",
                ),
                p => (
                    format!("自动修复没做成：{what}"),
                    format!("Repair not done: {what} ({p})"),
                    "warn",
                ),
            };
            ev(t, "repair", zh, en, tone)
        }
        _ => return None,
    })
}

/// Lines of a file, one at a time; a missing file is no lines.
fn lines(p: &Path) -> impl Iterator<Item = Value> {
    std::fs::File::open(p)
        .ok()
        .into_iter()
        .flat_map(|f| std::io::BufReader::new(f).lines())
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
}

/// The timeline at wall second `now` (`trusted`: the clock can be believed), from the long-term
/// directory `dir` and `/v2/incidents`' reply.
pub fn build(dir: &Path, now: i64, trusted: bool, incidents: &Value) -> Value {
    if !trusted {
        return json!({"available": true, "clock_trusted": false, "since": null, "events": [], "hours": []});
    }
    let since = now - now.rem_euclid(DAY_S);
    let today = |t: Option<i64>| t.filter(|t| (since..=now + 60).contains(t));
    let mut events: VecDeque<Value> = VecDeque::new();
    for v in lines(&dir.join("events.jsonl")) {
        let Some(t) = today(v["t"].as_i64()) else {
            continue;
        };
        let Some(e) = item(&v, t) else { continue };
        // a boot without its facts may be followed by one with them: keep the boot's last
        if e["kind"] == "boot" {
            events.retain(|x| !(x["kind"] == "boot" && x["boot"] == e["boot"]));
        }
        events.push_back(e);
        if events.len() > EVENTS_MAX {
            events.pop_front();
        }
    }
    for side in ["open", "closed"] {
        for i in incidents[side].as_array().into_iter().flatten() {
            let (zh, en) = (
                i["name_zh"].as_str().unwrap_or(""),
                i["name_en"].as_str().unwrap_or(""),
            );
            if let Some(t) = today(i["from"].as_i64()) {
                events.push_back(ev(
                    t,
                    "incident_open",
                    format!("{zh}：开始"),
                    format!("{en}: started"),
                    "bad",
                ));
            }
            if let Some(t) = today(i["to"].as_i64()) {
                events.push_back(ev(
                    t,
                    "incident_close",
                    format!("{zh}：结束"),
                    format!("{en}: ended"),
                    "ok",
                ));
            }
        }
    }
    let mut events: Vec<Value> = events.into();
    events.sort_by_key(|e| e["t"].as_i64());
    if events.len() > EVENTS_MAX {
        events.drain(..events.len() - EVENTS_MAX);
    }
    for e in &mut events {
        if let Some(o) = e.as_object_mut() {
            o.remove("boot");
        }
    }

    let mut hours: VecDeque<Value> = VecDeque::new();
    for v in lines(&dir.join("hours.jsonl")) {
        if v["k"] != "hour" {
            continue;
        }
        let Some(ft) = v["ft"].as_i64() else { continue };
        if !(since..=now).contains(&ft) {
            continue;
        }
        hours.push_back(json!({
            "hour": ft.rem_euclid(DAY_S) / 3600,
            "from": ft,
            "to": v["tt"],
            "sinr_avg": v["sinr_avg"],
            "rat": v["rat"],
            "link_down": v["link_down"],
            "batt_to": v["batt_to"],
            "chg_s": v["chg_s"],
            "abroad_s": v["abroad"],
        }));
        if hours.len() > HOURS_MAX {
            hours.pop_front();
        }
    }
    json!({
        "available": true,
        "clock_trusted": true,
        "since": since,
        "now": now,
        "events": events,
        "hours": Vec::from(hours),
    })
}

/// Where the handler reads from, fixed when the server starts (tests build their own).
pub struct Source {
    /// The long-term directory; `None` when the layer is off (`available: false`).
    pub dir: Option<PathBuf>,
    /// The incidents' directory (`regress::io::incidents`).
    pub alerts: PathBuf,
    /// `(wall second, trusted)`.
    pub clock: fn() -> (i64, bool),
}

impl Source {
    /// The device's: on with `LUYAZ_LONGTERM=1`, the host paths, the boot-aware clock.
    pub fn real() -> Source {
        Source {
            dir: super::enabled().then(|| PathBuf::from(crate::state::host_path(super::DIR))),
            alerts: PathBuf::from(crate::state::host_path(luyaz_api::alerts::DIR)),
            clock: || {
                let c = super::clock_now(&crate::evlog::boot_id());
                (c.wall, c.wall_ok)
            },
        }
    }

    /// The reply's `data` (blocking: reads files).
    pub fn data(&self) -> Value {
        let Some(dir) = &self.dir else {
            return json!({"available": false});
        };
        let (now, trusted) = (self.clock)();
        let incidents = super::regress::io::incidents(&self.alerts);
        build(dir, now, trusted, &incidents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("u60d-today-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(d: &Path, name: &str, lines: &[Value]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(d.join(name), text).unwrap();
    }

    const DAY: i64 = 20_000 * DAY_S;

    #[test]
    fn today_from_the_two_files_and_the_incidents() {
        let d = dir("all");
        let now = DAY + 15 * 3600;
        write(
            &d,
            "events.jsonl",
            &[
                json!({"t": DAY - 10, "k": "link", "state": "down"}),
                json!({"t": null, "raw_wall": DAY + 5, "k": "link", "state": "down"}),
                json!({"t": DAY + 60, "k": "boot", "boot": "aa", "boot_id": "x"}),
                json!({"t": DAY + 600, "k": "boot", "boot": "aa", "code": 1185}),
                json!({"t": DAY + 3600, "k": "link", "state": "down"}),
                json!({"t": DAY + 3700, "k": "link", "state": "up"}),
                json!({"t": DAY + 4000, "k": "stale", "block": "signal"}),
                json!({"t": DAY + 5000, "k": "scenario", "from": "away", "to": "home", "name": "在家", "ok": true}),
                json!({"t": DAY + 6000, "k": "charge", "state": "start"}),
                json!({"t": DAY + 7000, "k": "repair", "item": "net_mode", "phase": "rolled_back"}),
            ],
        );
        write(
            &d,
            "hours.jsonl",
            &[
                json!({"k": "hour", "ft": DAY - 3600, "tt": DAY, "sinr_avg": 1.0}),
                json!({"k": "hour", "ft": DAY + 3600, "tt": DAY + 7200, "sinr_avg": 12.5, "rat": "SA", "link_down": 1}),
                json!({"k": "hour_power", "ft": DAY + 3600}),
            ],
        );
        let inc = json!({"open": [{"name_zh": "CHILL 出口", "name_en": "CHILL exit", "from": DAY + 8000, "to": null}],
                         "closed": [{"name_zh": "旧的", "name_en": "old", "from": DAY - 99, "to": DAY + 9000}]});
        let v = build(&d, now, true, &inc);
        assert_eq!(v["since"], DAY);
        let kinds: Vec<&str> = v["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            [
                "boot",
                "link_down",
                "link_up",
                "scenario",
                "charge_start",
                "repair",
                "incident_open",
                "incident_close"
            ]
        );
        let e = &v["events"][0];
        assert_eq!(
            (e["code"].as_i64(), e["text_zh"].as_str()),
            (Some(1185), Some("开机（原因码 1185）"))
        );
        assert!(e.get("boot").is_none());
        assert_eq!(v["events"][3]["text_en"], "Scenario: 在家");
        assert_eq!(v["events"][5]["tone"], "warn");
        let h = v["hours"].as_array().unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(
            (h[0]["hour"].as_i64(), h[0]["sinr_avg"].as_f64()),
            (Some(1), Some(12.5))
        );
        assert_eq!(h[0]["rat"], "SA");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn untrusted_clock_lists_nothing_and_events_are_capped() {
        let d = dir("cap");
        let many: Vec<Value> = (0..250)
            .map(|i| json!({"t": DAY + i, "k": "link", "state": if i % 2 == 0 { "down" } else { "up" }}))
            .collect();
        write(&d, "events.jsonl", &many);
        let v = build(&d, DAY + 3600, false, &Value::Null);
        assert_eq!(
            (
                v["clock_trusted"].as_bool(),
                v["events"].as_array().map(Vec::len)
            ),
            (Some(false), Some(0))
        );
        let v = build(&d, DAY + 3600, true, &Value::Null);
        let ev = v["events"].as_array().unwrap();
        assert_eq!(ev.len(), EVENTS_MAX);
        assert_eq!(ev[0]["t"], DAY + 50, "the newest kept");
        // no files at all: an empty day
        let v = build(&dir("none"), DAY + 3600, true, &Value::Null);
        assert_eq!(v["events"], json!([]));
        let _ = std::fs::remove_dir_all(&d);
    }
}
