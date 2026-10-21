//! The touch screen's words for the read-only scenarios (DESIGN.md §4: the core decides the
//! wording, the touch UI only displays it). Only what is worth saying gets words; everything
//! else is `None` and shows nothing. The admin web keeps its longer text of its own.

use serde::Serialize;

use super::{hot_drain, long_idle, slow_signal};

/// One notice: a short label (health row) and one or two sentences under it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Say {
    pub id: &'static str,
    pub label: &'static str,
    pub label_en: &'static str,
    pub detail: String,
    pub detail_en: String,
}

/// The touch UI keeps at most this many bytes of a detail.
pub const DETAIL_MAX: usize = 320;

fn deg(dc: i64) -> String {
    format!("{:.1}", dc as f64 / 10.0)
}

/// `power.hot_drain`: hot and draining fast, or hot with the drain unread.
pub fn hot(e: &hot_drain::Evidence, a: &hot_drain::Advice) -> Option<Say> {
    let (t, n, ratio) = match a {
        hot_drain::Advice::Explain {
            batt_dc,
            batt_normal_dc,
            drain_mw,
            drain_normal_mw,
            ..
        } => (
            *batt_dc,
            *batt_normal_dc,
            Some(f64::from(*drain_mw) / f64::from((*drain_normal_mw).max(1))),
        ),
        hot_drain::Advice::Unknown {
            why: "drain_unqualified" | "no_drain_baseline",
        } => (e.batt_dc?, e.batt_normal_dc?, None),
        _ => return None,
    };
    let (t, n) = (deg(t), deg(n));
    let (zh, en) = match ratio {
        Some(r) => (
            format!("耗电是平时的 {r:.1} 倍。"),
            format!("drawing {r:.1}× usual."),
        ),
        None => (
            "耗电有没有变快判断不了。".to_string(),
            "can't tell if it drains faster.".to_string(),
        ),
    };
    Some(Say {
        id: "hot_drain",
        label: "电池发热",
        label_en: "Battery hot",
        detail: format!(
            "电池 {t} °C，同样情况平时不超过 {n} °C；{zh}没测室温，放在热处或包里也会这样。"
        ),
        detail_en: format!(
            "Battery {t} °C, usually at most {n} °C here; {en} Room temperature isn't measured."
        ),
    })
}

/// `usage.long_idle`: idle long enough and the CHILL idle pause is off.
pub fn idle(a: &long_idle::Advice) -> Option<Say> {
    let long_idle::Advice::Suggest { hours, .. } = a else {
        return None;
    };
    Some(Say {
        id: "long_idle",
        label: "长时间闲置",
        label_en: "Long idle",
        detail: format!(
            "已经 {hours} 小时没人用。在 core.env 加 LUYAZ_CHILL_IDLE=1 再重启核心，没人时会暂停 CHILL、有人就恢复。不会自动改。"
        ),
        detail_en: format!(
            "Unused for {hours} h. LUYAZ_CHILL_IDLE=1 in core.env (then restart the core) pauses CHILL while nobody's around. Nothing changes by itself."
        ),
    })
}

/// `perf.slow_strong_signal`: one line under the deep check's rows.
pub fn slow(a: &slow_signal::Advice) -> Option<Say> {
    let (detail, detail_en) = match a {
        slow_signal::Advice::Suggest { busy_s, .. } => {
            let m = (busy_s / 60).max(1);
            (
                format!("信号强、已连续传 {m} 分钟数据；觉得慢就点开始检查。"),
                format!("Strong signal, data moving for {m} min; slow? Run the check."),
            )
        }
        slow_signal::Advice::Explain { .. } => (
            "信号是强的，慢多半不在信号，看上面标出的几项。".to_string(),
            "Strong signal: the slowness is likely in the rows marked above.".to_string(),
        ),
        _ => return None,
    };
    Some(Say {
        id: "slow_signal",
        label: "信号强但网速慢",
        label_en: "Strong signal, still slow",
        detail,
        detail_en,
    })
}

#[cfg(test)]
mod tests;
