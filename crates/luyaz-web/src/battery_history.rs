//! The battery curves on /router/device: the last 24 hours and 7 days, drawn server-side from
//! u60d's `/battery/history` (the long-term layer's hour lines, one reading an hour). A gap in the
//! lines (device off, core away) breaks the curve instead of joining across it.

use serde_json::Value;

use crate::i18n::Lang;

const W: f64 = 592.0;
const H: f64 = 120.0;
const PAD_Y: f64 = 8.0;
const HOUR_S: i64 = 3600;
/// Longer than this between two hours is a gap in the record.
const GAP_S: i64 = 600;

pub struct Chart {
    pub title: &'static str,
    pub start: &'static str,
    pub summary: String,
    /// Gridlines: (y in the viewBox, label top as % of the height, label).
    pub ticks: Vec<(String, String, String)>,
    /// One `points` list per unbroken stretch.
    pub lines: Vec<String>,
    /// Hours on the charger: (x, width).
    pub charging: Vec<(String, String)>,
}

#[derive(Default)]
pub struct History {
    pub note: Option<String>,
    pub charts: Vec<Chart>,
}

impl History {
    pub fn is_empty(&self) -> bool {
        self.note.is_none() && self.charts.is_empty()
    }
}

struct Hour {
    from: i64,
    to: i64,
    batt_from: i64,
    batt_to: i64,
    chg_s: i64,
}

fn hours(data: &Value) -> Vec<Hour> {
    let Some(list) = data["hours"].as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|h| {
            Some(Hour {
                from: h["from"].as_i64()?,
                to: h["to"].as_i64()?,
                batt_from: h["batt_from"].as_i64()?,
                batt_to: h["batt_to"].as_i64()?,
                chg_s: h["chg_s"].as_i64().unwrap_or(0),
            })
        })
        .collect()
}

fn y(pct: i64) -> f64 {
    PAD_Y + (100 - pct.clamp(0, 100)) as f64 / 100.0 * (H - PAD_Y * 2.0)
}

fn chart(all: &[Hour], now: i64, span_s: i64, title: &'static str, start: &'static str, l: Lang) -> Option<Chart> {
    let first = now - span_s;
    let list: Vec<&Hour> = all.iter().filter(|h| h.from >= first).collect();
    if list.is_empty() {
        return None;
    }
    let x = |t: i64| (t - first) as f64 / span_s as f64 * W;
    let mut lines: Vec<String> = Vec::new();
    let mut prev_to = None;
    for h in &list {
        let joined = prev_to.is_some_and(|t: i64| h.from - t <= GAP_S);
        if !joined {
            lines.push(format!("{:.0},{:.0}", x(h.from), y(h.batt_from)));
        }
        if let Some(ln) = lines.last_mut() {
            ln.push_str(&format!(" {:.0},{:.0}", x(h.to), y(h.batt_to)));
        }
        prev_to = Some(h.to);
    }
    let charging = list
        .iter()
        .filter(|h| h.chg_s > 0)
        .map(|h| (format!("{:.1}", x(h.from)), format!("{:.1}", x(h.to) - x(h.from))))
        .collect();
    let low = list.iter().map(|h| h.batt_from.min(h.batt_to)).min()?;
    let chg_h = list.iter().map(|h| h.chg_s).sum::<i64>() as f64 / HOUR_S as f64;
    let summary = if chg_h >= 0.05 {
        format!(
            "{} {low}%{}{} {chg_h:.1} {}",
            l.pick("lowest", "最低"),
            l.pick(", ", "，"),
            l.pick("charged", "充电"),
            l.pick("h", "小时")
        )
    } else {
        format!("{} {low}%", l.pick("lowest", "最低"))
    };
    let ticks = [0, 50, 100]
        .iter()
        .map(|&p| {
            let v = y(p);
            (format!("{v:.1}"), format!("{:.2}", v / H * 100.0), format!("{p}"))
        })
        .collect();
    Some(Chart { title, start, summary, ticks, lines, charging })
}

/// The section for u60d's answer. `failed` with no answer: an older core or a failed read says
/// nothing rather than a wrong "no records".
pub fn view(data: Option<&Value>, failed: bool, l: Lang) -> History {
    let say = |en, zh| History { note: Some(l.pick(en, zh).to_string()), charts: Vec::new() };
    let Some(d) = data else {
        return if failed {
            say("Can't read the battery history right now.", "暂时读不到电量曲线。")
        } else {
            History::default()
        };
    };
    if d["available"].as_bool() != Some(true) {
        return say(
            "No history on this device: long-term records are off (LUYAZ_LONGTERM=1 in /data/luyaz/core/core.env turns them on).",
            "这台设备没有电量曲线：长期记录没开（在 /data/luyaz/core/core.env 加 LUYAZ_LONGTERM=1 可开启）。",
        );
    }
    if d["clock_trusted"].as_bool() != Some(true) {
        return say(
            "The battery history shows once the device's clock is set.",
            "设备时间校准后才显示电量曲线。",
        );
    }
    let (list, Some(now)) = (hours(d), d["now"].as_i64()) else {
        return say("Can't read the battery history right now.", "暂时读不到电量曲线。");
    };
    let charts: Vec<Chart> = [
        chart(&list, now, 24 * HOUR_S, l.pick("Last 24 hours", "近 24 小时"), l.pick("24 h ago", "24 小时前"), l),
        chart(&list, now, 7 * 24 * HOUR_S, l.pick("Last 7 days", "近 7 天"), l.pick("7 days ago", "7 天前"), l),
    ]
    .into_iter()
    .flatten()
    .collect();
    if charts.is_empty() {
        return say(
            "Nothing recorded yet: the first reading comes after a full hour.",
            "还没有记录：过满一小时会有第一个点。",
        );
    }
    History { note: None, charts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_000_000;

    fn h(from: i64, a: i64, b: i64, chg: i64) -> Value {
        json!({"from": from, "to": from + 3600, "batt_from": a, "batt_to": b, "chg_s": chg})
    }

    #[test]
    fn a_gap_breaks_the_line_and_charging_hours_are_shaded() {
        let d = json!({"available": true, "clock_trusted": true, "now": NOW, "hours": [
            h(NOW - 4 * 3600, 90, 80, 0),
            h(NOW - 3 * 3600, 80, 70, 1800),
            h(NOW - 3600, 70, 60, 0),
        ]});
        let v = view(Some(&d), false, Lang::Zh);
        assert_eq!(v.charts.len(), 2);
        let day = &v.charts[0];
        assert_eq!(day.lines.len(), 2, "{:?}", day.lines);
        assert_eq!(day.lines[0].split(' ').count(), 3);
        assert_eq!(day.charging.len(), 1);
        assert!(day.summary.contains("60%") && day.summary.contains("0.5"), "{}", day.summary);
    }

    #[test]
    fn the_short_chart_only_has_its_own_window() {
        let d = json!({"available": true, "clock_trusted": true, "now": NOW, "hours": [h(NOW - 3 * 86_400, 90, 80, 0)]});
        let v = view(Some(&d), false, Lang::Zh);
        assert_eq!(v.charts.len(), 1);
        assert_eq!(v.charts[0].start, "7 天前");
    }

    #[test]
    fn each_empty_case_says_why() {
        let off = view(Some(&json!({"available": false})), false, Lang::Zh);
        assert!(off.note.unwrap().contains("LUYAZ_LONGTERM"));
        let clock = view(Some(&json!({"available": true, "clock_trusted": false})), false, Lang::Zh);
        assert!(clock.note.unwrap().contains("校准"));
        let none = view(Some(&json!({"available": true, "clock_trusted": true, "now": NOW, "hours": []})), false, Lang::Zh);
        assert!(none.note.unwrap().contains("一小时"));
        assert!(view(None, false, Lang::Zh).is_empty());
        assert!(view(None, true, Lang::Zh).note.is_some());
    }
}
